use std::time::Duration;

use futures::pin_mut;
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::Sender;
use tokio::task::block_in_place;
use tokio::{select, sync::Mutex};
use trustification_event_bus::EventBus;
use trustification_index::{Index, IndexStore, IndexWriter};
use trustification_storage::{EventType, Storage};

pub mod actix;

#[derive(Clone, Debug)]
pub enum IndexerStatus {
    Running,
    Reindexing { progress: usize },
    Failed { error: String },
}

pub enum IndexerCommand {
    Reindex,
}

pub struct Indexer<'a, INDEX: Index> {
    pub stored_topic: &'a str,
    pub indexed_topic: &'a str,
    pub failed_topic: &'a str,
    pub sync_interval: Duration,
    pub index: IndexStore<INDEX>,
    pub storage: Storage,
    pub bus: EventBus,
    pub status: Arc<Mutex<IndexerStatus>>,
    pub commands: Receiver<IndexerCommand>,
    pub command_sender: Sender<IndexerCommand>,
}

impl<'a, INDEX: Index> Indexer<'a, INDEX> {
    pub async fn run(&mut self) -> Result<(), anyhow::Error> {
        // Load initial index from storage.
        if let Err(e) = self.index.sync(&self.storage).await {
            log::info!("Error loading initial index: {:?}", e);
        }

        let mut interval = tokio::time::interval(self.sync_interval);
        let mut writer = Some(block_in_place(|| self.index.writer())?);
        let consumer = self.bus.subscribe("indexer", &[self.stored_topic]).await?;
        let mut uncommitted_events = Vec::new();
        let mut events = 0;

        *self.status.lock().await = IndexerStatus::Running;
        loop {
            let tick = interval.tick();
            pin_mut!(tick);
            select! {
                command = self.commands.recv() => {
                    if let Some(IndexerCommand::Reindex) = command {
                        log::info!("Reindexing all documents");
                        let mut progress = 0;
                        *self.status.lock().await = IndexerStatus::Reindexing { progress };
                        match self.storage.list_all_objects().await {
                            Ok(objects) => {
                                pin_mut!(objects);

                                let mut interval = tokio::time::interval(self.sync_interval);
                                loop {
                                    let tick = interval.tick();
                                    pin_mut!(tick);
                                    select! {
                                        next = objects.next() => {
                                            match next {
                                                Some(Ok((key, obj))) => {
                                                    log::info!("Reindexing {}", key);
                                                    if let Err(e) = self.index_doc(self.index.index(), writer.as_mut().unwrap(), &key, &obj).await {
                                                        log::warn!("(Ignored) Internal error when indexing {}: {:?}", key, e);
                                                    } else {
                                                        progress += 1;
                                                        *self.status.lock().await = IndexerStatus::Reindexing { progress };
                                                    }
                                                }
                                                Some(Err(e)) => {
                                                    log::warn!("(Ignored) Error reindexing: {:?}", e);
                                                }
                                                _ => break,
                                            }
                                        }
                                        _ = tick => {
                                            match self.index.snapshot(writer.take().unwrap(), &self.storage, true).await {
                                                Ok(_) => {
                                                    log::trace!("Index snapshot published");
                                                }
                                                Err(e) => {
                                                    log::warn!("(Ignored) Error committing index: {:?}", e);
                                                }
                                            }
                                            writer.replace(block_in_place(|| self.index.writer())?);
                                        }
                                    }
                                }
                                log::info!("Reindexing finished");
                                match self.index.snapshot(writer.take().unwrap(), &self.storage, true).await {
                                    Ok(_) => {
                                        log::info!("Reindex index published");
                                    }
                                    Err(e) => {
                                        log::warn!("(Ignored) Error committing index: {:?}", e);
                                    }
                                }
                                writer.replace(block_in_place(|| self.index.writer())?);
                                *self.status.lock().await = IndexerStatus::Running;
                            }
                            Err(e) => {
                                log::warn!("Reindexing failed: {:?}", e);
                                *self.status.lock().await = IndexerStatus::Failed{ error: e.to_string() };
                            }
                        }
                    }
                }
                event = consumer.next() => match event {
                    Ok(Some(event)) => {
                        if let Some(payload) = event.payload() {
                            if let Ok(data) = self.storage.decode_event(payload) {
                                for data in data.records {
                                    if self.storage.is_index(data.key()) {
                                        log::trace!("It's an index event, ignoring");
                                    } else {
                                        match data.event_type() {
                                            EventType::Put => {
                                                match self.storage.get_for_event(&data).await {
                                                    Ok((k, data)) => {
                                                        if let Err(e) = self.index_doc(self.index.index(), writer.as_mut().unwrap(), &k, &data).await {
                                                            log::warn!("(Ignored) Internal error when indexing {}: {:?}", k, e);
                                                        }
                                                        events += 1;
                                                    }
                                                    Err(e) => {
                                                        log::warn!("Error retrieving document event data, ignoring (error: {:?})", e);
                                                    }
                                                }
                                            },
                                            EventType::Delete => {
                                                let (_, key) = Storage::key_from_event(&data)?;
                                                block_in_place(|| writer.as_mut().unwrap().delete_document(self.index.index(), key.as_str()));
                                                log::debug!("Deleted entry '{key}' from index");
                                                events += 1;
                                            }
                                            _ => log::debug!("Non (PUT | DELETE)  event ({:?}), skipping", data),
                                        }
                                    }
                                }
                            } else {
                                log::warn!("Error decoding event, skipping");
                            }
                        } else {
                            log::warn!("No event for payload, skipping");
                        }
                        uncommitted_events.push(event);
                    }
                    Ok(None) => {
                        log::debug!("Polling returned no events, retrying");
                    }
                    Err(e) => {
                        log::warn!("Error polling for event: {:?}", e);
                    }
                },
                _ = tick => {
                    log::trace!("{} new events added, pushing new index to storage", events);
                    match self.index.snapshot(writer.take().unwrap(), &self.storage, events > 0).await {
                        Ok(_) => {
                            log::trace!("Index updated successfully");
                            match consumer.commit(&uncommitted_events[..]).await {
                                Ok(_) => {
                                    log::trace!("Event committed successfully");
                                    uncommitted_events.clear();
                                }
                                Err(e) => {
                                    log::warn!("Error committing event: {:?}", e)
                                }
                            }
                            events = 0;
                        }
                        Err(e) => {
                            log::warn!("Error taking index snapshot: {:?}", e);
                        }
                    }
                    writer.replace(block_in_place(|| self.index.writer())?);
                }
            }
        }
    }

    async fn index_doc(
        &self,
        index: &INDEX,
        writer: &mut IndexWriter,
        key: &str,
        data: &[u8],
    ) -> Result<(), anyhow::Error> {
        match INDEX::parse_doc(data) {
            Ok(doc) => match block_in_place(|| writer.add_document(index, key, &doc)) {
                Ok(_) => {
                    log::debug!("Inserted entry '{key}' into index");
                    self.bus.send(self.indexed_topic, key.as_bytes()).await?;
                }
                Err(e) => {
                    let failure = serde_json::json!( {
                        "key": key,
                        "error": e.to_string(),
                    })
                    .to_string();
                    self.bus.send(self.failed_topic, failure.as_bytes()).await?;
                    log::warn!("Error inserting entry '{key}' into index: {e:?}")
                }
            },
            Err(e) => {
                log::warn!("Error parsing document for key '{key}': {e:?}");
                let failure = serde_json::json!( {
                    "key": key,
                    "error": e.to_string(),
                })
                .to_string();
                self.bus.send(self.failed_topic, failure.as_bytes()).await?;
            }
        }
        Ok(())
    }
}
