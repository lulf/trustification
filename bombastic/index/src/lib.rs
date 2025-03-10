use core::str::FromStr;

use bombastic_model::prelude::*;
use cyclonedx_bom::models::{
    component::Classification,
    hash::HashAlgorithm,
    license::{LicenseChoice, LicenseIdentifier},
};
use log::{debug, info, warn};
use sikula::{mir::Direction, prelude::*};
use spdx_rs::models::Algorithm;
use tantivy::collector::TopDocs;
use tantivy::query::TermSetQuery;
use tantivy::{
    query::{AllQuery, BooleanQuery},
    schema::INDEXED,
    store::ZstdCompressor,
    DocAddress, IndexSettings, Searcher, SnippetGenerator,
};
use time::format_description::well_known::Rfc3339;
use trustification_api::search::SearchOptions;
use trustification_index::{
    boost, create_boolean_query, create_date_query, create_string_query, create_text_query, field2str, field2strvec,
    metadata::doc2metadata,
    tantivy::{
        doc,
        query::{Occur, Query},
        schema::{Field, Schema, Term, FAST, STORED, STRING, TEXT},
        DateTime,
    },
    term2query, Document, Error as SearchError,
};

pub struct Index {
    schema: Schema,
    fields: Fields,
}

pub struct PackageFields {
    name: Field,
    version: Field,
    desc: Field,
    purl: Field,
    cpe: Field,
    license: Field,
    supplier: Field,
    classifier: Field,
    sha256: Field,
    purl_type: Field,
    purl_name: Field,
    purl_namespace: Field,
    purl_version: Field,
    purl_qualifiers: Field,
    purl_qualifiers_values: Field,
}

struct Fields {
    sbom_id: Field,
    sbom_created: Field,
    sbom_created_inverse: Field,
    sbom_creators: Field,
    sbom_name: Field,
    sbom: PackageFields,
    dep: PackageFields,
}

impl Default for Index {
    fn default() -> Self {
        Self::new()
    }
}

impl Index {
    pub fn new() -> Self {
        let mut schema = Schema::builder();
        let fields = Fields {
            sbom_id: schema.add_text_field("sbom_id", STRING | FAST | STORED),
            sbom_created: schema.add_date_field("sbom_created", INDEXED | FAST | STORED),
            sbom_created_inverse: schema.add_date_field("sbom_created_inverse", FAST),
            sbom_creators: schema.add_text_field("sbom_creators", STRING | STORED),
            sbom_name: schema.add_text_field("sbom_name", STRING | FAST | STORED),
            sbom: PackageFields {
                name: schema.add_text_field("sbom_pkg_name", STRING | FAST | STORED),
                version: schema.add_text_field("sbom_pkg_version", STRING | STORED),
                purl: schema.add_text_field("sbom_pkg_purl", STRING | FAST | STORED),
                desc: schema.add_text_field("sbom_pkg_desc", TEXT | STORED),
                license: schema.add_text_field("sbom_pkg_license", TEXT | STORED),
                cpe: schema.add_text_field("sbom_pkg_cpe", STRING | FAST | STORED),
                supplier: schema.add_text_field("sbom_pkg_supplier", STRING | STORED),
                classifier: schema.add_text_field("sbom_pkg_classifier", STRING),
                sha256: schema.add_text_field("sbom_pkg_sha256", STRING | STORED),
                purl_type: schema.add_text_field("sbom_pkg_purl_type", STRING),
                purl_name: schema.add_text_field("sbom_pkg_purl_name", FAST | STRING),
                purl_namespace: schema.add_text_field("sbom_pkg_purl_namespace", STRING),
                purl_version: schema.add_text_field("sbom_pkg_purl_version", STRING),
                purl_qualifiers: schema.add_text_field("sbom_pkg_purl_qualifiers", STRING),
                purl_qualifiers_values: schema.add_text_field("sbom_pkg_purl_qualifiers_values", STRING),
            },
            dep: PackageFields {
                name: schema.add_text_field("dep_name", FAST | STRING),
                purl: schema.add_text_field("dep_purl", FAST | STRING | STORED),
                version: schema.add_text_field("dep_version", STRING),
                desc: schema.add_text_field("dep_desc", TEXT),
                cpe: schema.add_text_field("dep_cpe", STRING | FAST | STORED),
                license: schema.add_text_field("dep_license", TEXT | STORED),
                supplier: schema.add_text_field("dep_supplier", STRING),
                classifier: schema.add_text_field("dep_classifier", STRING),
                sha256: schema.add_text_field("dep_sha256", STRING),
                purl_type: schema.add_text_field("dep_purl_type", STRING),
                purl_name: schema.add_text_field("dep_purl_name", FAST | STRING),
                purl_namespace: schema.add_text_field("dep_purl_namespace", STRING),
                purl_version: schema.add_text_field("dep_purl_version", STRING),
                purl_qualifiers: schema.add_text_field("dep_purl_qualifiers", STRING),
                purl_qualifiers_values: schema.add_text_field("dep_purl_qualifiers_values", STRING),
            },
        };
        Self {
            schema: schema.build(),
            fields,
        }
    }

    fn index_spdx(&self, id: &str, bom: &spdx_rs::models::SPDX) -> Result<Vec<Document>, SearchError> {
        debug!("Indexing SPDX document");

        let mut document = doc!();

        document.add_text(self.fields.sbom_id, id);
        document.add_text(self.fields.sbom_name, &bom.document_creation_information.document_name);

        for creators in &bom.document_creation_information.creation_info.creators {
            document.add_text(self.fields.sbom_creators, creators);
        }

        let created = &bom.document_creation_information.creation_info.created;
        document.add_date(
            self.fields.sbom_created,
            DateTime::from_timestamp_millis(created.timestamp_millis()),
        );

        document.add_date(
            self.fields.sbom_created_inverse,
            DateTime::from_timestamp_millis(-created.timestamp_millis()),
        );

        for package in &bom.package_information {
            if bom
                .document_creation_information
                .document_describes
                .contains(&package.package_spdx_identifier)
            {
                info!("Indexing SBOM {} with name {}", id, package.package_name);
                Self::index_spdx_package(&mut document, package, &self.fields.sbom);
            } else {
                Self::index_spdx_package(&mut document, package, &self.fields.dep);
            }
        }
        debug!("Indexed {:?}", document);
        Ok(vec![document])
    }

    fn index_spdx_package(
        document: &mut Document,
        package: &spdx_rs::models::PackageInformation,
        fields: &PackageFields,
    ) {
        if let Some(comment) = &package.package_summary_description {
            document.add_text(fields.desc, comment);
        }
        for r in package.external_reference.iter() {
            if r.reference_type == "cpe22Type" {
                document.add_text(fields.cpe, &r.reference_locator);
            }
            if r.reference_type == "purl" {
                let purl = r.reference_locator.clone();
                document.add_text(fields.purl, &purl);

                if let Ok(package) = packageurl::PackageUrl::from_str(&purl) {
                    document.add_text(fields.purl_name, package.name());
                    if let Some(namespace) = package.namespace() {
                        document.add_text(fields.purl_namespace, namespace);
                    }

                    if let Some(version) = package.version() {
                        document.add_text(fields.purl_version, version);
                    }

                    for entry in package.qualifiers().iter() {
                        document.add_text(fields.purl_qualifiers, format!("{}={}", entry.0, entry.1));
                        document.add_text(fields.purl_qualifiers_values, entry.1);
                    }

                    document.add_text(fields.purl_type, package.ty());
                }
            }
        }

        document.add_text(fields.name, &package.package_name);
        if let Some(version) = &package.package_version {
            document.add_text(fields.version, version);
        }

        for sum in package.package_checksum.iter() {
            if sum.algorithm == Algorithm::SHA256 {
                document.add_text(fields.sha256, &sum.value);
            }
        }

        document.add_text(fields.license, package.declared_license.to_string());
        if let Some(supplier) = &package.package_supplier {
            document.add_text(fields.supplier, supplier);
        }
    }

    fn index_cyclonedx(&self, id: &str, bom: &cyclonedx_bom::prelude::Bom) -> Result<Vec<Document>, SearchError> {
        let mut document = doc!();

        document.add_text(self.fields.sbom_id, id);
        if let Some(metadata) = &bom.metadata {
            if let Some(timestamp) = &metadata.timestamp {
                let timestamp = timestamp.to_string();
                if let Ok(d) = time::OffsetDateTime::parse(&timestamp, &Rfc3339) {
                    document.add_date(
                        self.fields.sbom_created,
                        DateTime::from_timestamp_secs(d.unix_timestamp()),
                    );

                    document.add_date(
                        self.fields.sbom_created_inverse,
                        DateTime::from_timestamp_secs(-d.unix_timestamp()),
                    );
                }
            }

            if let Some(component) = &metadata.component {
                document.add_text(self.fields.sbom_name, component.name.to_string());
                Self::index_cyclonedx_component(&mut document, component, &self.fields.sbom);
            }
        }

        if let Some(components) = &bom.components {
            for component in components.0.iter() {
                Self::index_cyclonedx_component(&mut document, component, &self.fields.dep);
            }
        }
        Ok(vec![document])
    }

    fn index_cyclonedx_component(
        document: &mut Document,
        component: &cyclonedx_bom::prelude::Component,
        fields: &PackageFields,
    ) {
        if let Some(hashes) = &component.hashes {
            for hash in hashes.0.iter() {
                if hash.alg == HashAlgorithm::SHA256 {
                    document.add_text(fields.sha256, &hash.content.0);
                }
            }
        }

        document.add_text(fields.name, component.name.to_string());
        document.add_text(fields.version, component.version.to_string());

        if let Some(purl) = &component.purl {
            let purl = purl.to_string();
            document.add_text(fields.purl, &purl);

            if let Ok(package) = packageurl::PackageUrl::from_str(&purl) {
                document.add_text(fields.purl_name, package.name());
                if let Some(namespace) = package.namespace() {
                    document.add_text(fields.purl_namespace, namespace);
                }

                if let Some(version) = package.version() {
                    document.add_text(fields.purl_version, version);
                }

                for entry in package.qualifiers().iter() {
                    document.add_text(fields.purl_qualifiers, entry.1);
                }
                document.add_text(fields.purl_type, package.ty());
            }
        }

        if let Some(desc) = &component.description {
            document.add_text(fields.desc, desc.to_string());
        }

        if let Some(licenses) = &component.licenses {
            licenses.0.iter().for_each(|l| match l {
                LicenseChoice::License(l) => match &l.license_identifier {
                    LicenseIdentifier::Name(s) => {
                        document.add_text(fields.license, s.to_string());
                    }
                    LicenseIdentifier::SpdxId(_) => (),
                },
                LicenseChoice::Expression(_) => (),
            });
        }

        document.add_text(fields.classifier, component.component_type.to_string());
    }

    fn resource2query(&self, resource: &Packages) -> Box<dyn Query> {
        const PACKAGE_WEIGHT: f32 = 1.5;
        const CREATED_WEIGHT: f32 = 1.25;
        match resource {
            Packages::Package(primary) => boost(
                self.create_string_query(
                    &[
                        self.fields.sbom_name,
                        self.fields.sbom.name,
                        self.fields.sbom.purl,
                        self.fields.sbom.cpe,
                        self.fields.sbom.purl_name,
                    ],
                    primary,
                ),
                PACKAGE_WEIGHT,
            ),

            Packages::Type(value) => Box::new(TermSetQuery::new(vec![Term::from_field_text(
                self.fields.sbom.purl_type,
                value,
            )])),

            Packages::Namespace(value) => Box::new(TermSetQuery::new(vec![Term::from_field_text(
                self.fields.sbom.purl_namespace,
                value,
            )])),

            Packages::Created(ordered) => boost(create_date_query(self.fields.sbom_created, ordered), CREATED_WEIGHT),

            Packages::Version(value) => {
                self.create_string_query(&[self.fields.sbom.version, self.fields.sbom.purl_version], value)
            }

            Packages::Description(primary) => self.create_text_query(&[self.fields.sbom.desc], primary),

            Packages::Digest(value) => Box::new(TermSetQuery::new(vec![Term::from_field_text(
                self.fields.sbom.sha256,
                value,
            )])),

            Packages::License(value) => Box::new(TermSetQuery::new(vec![Term::from_field_text(
                self.fields.sbom.license,
                value,
            )])),

            Packages::Supplier(primary) => self.create_string_query(&[self.fields.sbom.supplier], primary),

            Packages::Qualifier(qualified) => {
                let mut qs = Vec::new();
                for qualifier in qualified.qualifier.0.iter() {
                    let exp = format!("{}={}", qualifier, qualified.expression);
                    let q = self.create_string_query(&[self.fields.sbom.purl_qualifiers], &Primary::Equal(&exp));
                    qs.push(q);
                }
                Box::new(BooleanQuery::union(qs))
            }

            Packages::Dependency(primary) => self.create_string_query(
                &[
                    self.fields.dep.name,
                    self.fields.dep.purl_name,
                    self.fields.dep.purl,
                    self.fields.dep.cpe,
                ],
                primary,
            ),

            Packages::Application => self.match_classifiers(Classification::Application),
            Packages::Library => self.match_classifiers(Classification::Library),
            Packages::Framework => self.match_classifiers(Classification::Framework),
            Packages::Container => self.match_classifiers(Classification::Container),
            Packages::OperatingSystem => self.match_classifiers(Classification::OperatingSystem),
            Packages::Device => self.match_classifiers(Classification::Device),
            Packages::Firmware => self.match_classifiers(Classification::Firmware),
            Packages::File => self.match_classifiers(Classification::File),
        }
    }

    fn create_string_query(&self, fields: &[Field], value: &Primary<'_>) -> Box<dyn Query> {
        let queries: Vec<Box<dyn Query>> = fields.iter().map(|f| create_string_query(*f, value)).collect();
        Box::new(BooleanQuery::union(queries))
    }

    fn create_text_query(&self, fields: &[Field], value: &Primary<'_>) -> Box<dyn Query> {
        let queries: Vec<Box<dyn Query>> = fields.iter().map(|f| create_text_query(*f, value)).collect();
        Box::new(BooleanQuery::union(queries))
    }

    fn match_classifiers(&self, classification: Classification) -> Box<dyn Query> {
        Box::new(BooleanQuery::union(vec![
            create_boolean_query(
                Occur::Should,
                Term::from_field_text(self.fields.sbom.classifier, &classification.to_string()),
            ),
            create_boolean_query(
                Occur::Should,
                Term::from_field_text(self.fields.dep.classifier, &classification.to_string()),
            ),
        ]))
    }
}

#[derive(Debug)]
pub struct SbomQuery {
    query: Box<dyn Query>,
    sort_by: Option<Field>,
}

impl trustification_index::Index for Index {
    type MatchedDocument = SearchHit;
    type Document = SBOM;
    type QueryContext = SbomQuery;

    fn index_doc(&self, id: &str, doc: &SBOM) -> Result<Vec<Document>, SearchError> {
        match doc {
            SBOM::CycloneDX(bom) => self.index_cyclonedx(id, bom),
            SBOM::SPDX(bom) => self.index_spdx(id, bom),
        }
    }

    fn parse_doc(data: &[u8]) -> Result<SBOM, SearchError> {
        SBOM::parse(data).map_err(|e| SearchError::DocParser(e.to_string()))
    }

    fn schema(&self) -> Schema {
        self.schema.clone()
    }

    fn settings(&self) -> IndexSettings {
        IndexSettings {
            docstore_compression: tantivy::store::Compressor::Zstd(ZstdCompressor::default()),
            ..Default::default()
        }
    }

    fn doc_id_to_term(&self, id: &str) -> Term {
        self.schema
            .get_field("sbom_id")
            .map(|f| Term::from_field_text(f, id))
            .unwrap()
    }

    fn prepare_query(&self, q: &str) -> Result<SbomQuery, SearchError> {
        if q.is_empty() {
            return Ok(SbomQuery {
                query: Box::new(AllQuery),
                sort_by: None,
            });
        }

        let mut query = Packages::parse(q).map_err(|err| SearchError::QueryParser(err.to_string()))?;

        query.term = query.term.compact();

        debug!("Query: {query:?}");

        let mut sort_by = None;
        if let Some(f) = query.sorting.first() {
            match f.qualifier {
                PackagesSortable::Created => match f.direction {
                    Direction::Descending => {
                        sort_by.replace(self.fields.sbom_created);
                    }
                    Direction::Ascending => {
                        sort_by.replace(self.fields.sbom_created_inverse);
                    }
                },
            }
        }

        let query = term2query(&query.term, &|resource| self.resource2query(resource));

        debug!("Processed query: {:?}", query);
        Ok(SbomQuery { query, sort_by })
    }

    fn search(
        &self,
        searcher: &Searcher,
        query: &SbomQuery,
        offset: usize,
        limit: usize,
    ) -> Result<(Vec<(f32, DocAddress)>, usize), SearchError> {
        if let Some(order_by) = query.sort_by {
            let mut hits = Vec::new();
            let result = searcher.search(
                &query.query,
                &(
                    TopDocs::with_limit(limit)
                        .and_offset(offset)
                        .order_by_fast_field::<tantivy::DateTime>(order_by),
                    tantivy::collector::Count,
                ),
            )?;
            for r in result.0 {
                hits.push((1.0, r.1));
            }
            Ok((hits, result.1))
        } else {
            Ok(searcher.search(
                &query.query,
                &(TopDocs::with_limit(limit).and_offset(offset), tantivy::collector::Count),
            )?)
        }
    }

    fn process_hit(
        &self,
        doc_address: DocAddress,
        score: f32,
        searcher: &Searcher,
        query: &SbomQuery,
        options: &SearchOptions,
    ) -> Result<Self::MatchedDocument, SearchError> {
        let doc = searcher.doc(doc_address)?;
        let id = field2str(&doc, self.fields.sbom_id)?;
        let name = field2str(&doc, self.fields.sbom_name)?;

        let snippet_generator = SnippetGenerator::create(searcher, &query.query, self.fields.sbom.desc)?;
        let snippet = snippet_generator.snippet_from_doc(&doc).to_html();

        let purl = doc
            .get_first(self.fields.sbom.purl)
            .map(|s| s.as_text().unwrap_or(""))
            .map(|s| s.to_string());

        let cpe = doc
            .get_first(self.fields.sbom.cpe)
            .map(|s| s.as_text().unwrap_or(""))
            .map(|s| s.to_string());

        let version = doc
            .get_first(self.fields.sbom.version)
            .map(|s| s.as_text().unwrap_or(""))
            .unwrap_or("");

        let sha256 = doc
            .get_first(self.fields.sbom.sha256)
            .map(|s| s.as_text().unwrap_or(""))
            .unwrap_or("");

        let license = doc
            .get_first(self.fields.sbom.license)
            .map(|s| s.as_text().unwrap_or("Unknown"))
            .unwrap_or("Unknown");

        let classifier = doc
            .get_first(self.fields.sbom.classifier)
            .map(|s| s.as_text().unwrap_or("Unknown"))
            .unwrap_or("Unknown");

        let supplier = doc
            .get_first(self.fields.sbom.supplier)
            .map(|s| s.as_text().unwrap_or("Unknown"))
            .unwrap_or("Unknown");

        let description = doc
            .get_first(self.fields.sbom.desc)
            .map(|s| s.as_text().unwrap_or(name))
            .unwrap_or(name);

        let created: time::OffsetDateTime = doc
            .get_first(self.fields.sbom_created)
            .map(|s| {
                s.as_date()
                    .map(|d| d.into_utc())
                    .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
            })
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);

        let dependencies: Vec<String> = field2strvec(&doc, self.fields.dep.purl)?
            .iter()
            .map(|s| s.to_string())
            .collect();

        let document = SearchDocument {
            id: id.to_string(),
            version: version.to_string(),
            purl,
            cpe,
            name: name.to_string(),
            sha256: sha256.to_string(),
            license: license.to_string(),
            classifier: classifier.to_string(),
            supplier: supplier.to_string(),
            snippet,
            created,
            description: description.to_string(),
            dependencies,
        };

        let explanation: Option<serde_json::Value> = if options.explain {
            match query.query.explain(searcher, doc_address) {
                Ok(explanation) => Some(serde_json::to_value(explanation).ok()).unwrap_or(None),
                Err(e) => {
                    warn!("Error producing explanation for document {:?}: {:?}", doc_address, e);
                    None
                }
            }
        } else {
            None
        };

        let metadata = options.metadata.then(|| doc2metadata(&self.schema, &doc));

        Ok(SearchHit {
            document,
            score,
            explanation,
            metadata,
        })
    }
}

#[cfg(test)]
mod tests {
    use trustification_index::IndexStore;

    use super::*;

    fn assert_search<F>(f: F)
    where
        F: FnOnce(IndexStore<Index>),
    {
        let _ = env_logger::try_init();

        let index = Index::new();
        let mut store = IndexStore::new_in_memory(index).unwrap();
        let mut writer = store.writer().unwrap();

        let data = std::fs::read_to_string("../testdata/ubi9-sbom.json").unwrap();
        let sbom = SBOM::parse(data.as_bytes()).unwrap();
        writer.add_document(store.index_as_mut(), "ubi9-sbom", &sbom).unwrap();

        let data = std::fs::read_to_string("../testdata/kmm-1.json").unwrap();
        let sbom = SBOM::parse(data.as_bytes()).unwrap();
        writer.add_document(store.index_as_mut(), "kmm-1", &sbom).unwrap();

        let data = std::fs::read_to_string("../testdata/my-sbom.json").unwrap();
        let sbom = SBOM::parse(data.as_bytes()).unwrap();
        writer.add_document(store.index_as_mut(), "my-sbom", &sbom).unwrap();

        writer.commit().unwrap();

        f(store);
    }

    fn search(index: &IndexStore<Index>, query: &str) -> (Vec<SearchHit>, usize) {
        index
            .search(
                query,
                0,
                10000,
                SearchOptions {
                    metadata: false,
                    explain: false,
                },
            )
            .unwrap()
    }

    #[tokio::test]
    async fn test_search_form() {
        assert_search(|index| {
            let result = search(&index, "ubi9-container");
            assert_eq!(result.0.len(), 1);
        });
    }

    #[tokio::test]
    async fn test_search_package() {
        assert_search(|index| {
            let result =
                search(&index,
"\"pkg:oci/ubi9@sha256:cb303404e576ff5528d4f08b12ad85fab8f61fa9e5dba67b37b119db24865df3?repository_url=registry.redhat.io/ubi9&tag=9.1.0-1782\" in:package"
                );
            assert_eq!(result.0.len(), 1);

            let result = search(&index, "ubi9-container in:package");
            assert_eq!(result.0.len(), 1);

            let result = search(&index, "ubi9-containe in:package");
            assert_eq!(result.0.len(), 1);

            let result = search(&index, "ubi9-containe in:package");
            assert_eq!(result.0.len(), 1);

            let result = search(&index, "\"cpe:/a:redhat:kernel_module_management:1.0::el9\" in:package");
            assert_eq!(result.0.len(), 1);
        });
    }

    #[tokio::test]
    async fn test_search_namespace() {
        assert_search(|index| {
            let result = search(&index, "namespace:io.seedwing");
            assert_eq!(result.0.len(), 1);
        });
    }

    #[tokio::test]
    async fn test_search_created() {
        assert_search(|index| {
            let result = search(&index, "created:>2022-01-01");
            assert_eq!(result.0.len(), 3);

            let result = search(&index, "created:2023-03-30");
            assert_eq!(result.0.len(), 1);

            let result = search(&index, "created:2023-03-29");
            assert_eq!(result.0.len(), 0);

            let result = search(&index, "created:2023-03-31");
            assert_eq!(result.0.len(), 0);
        });
    }

    #[tokio::test]
    async fn test_all() {
        assert_search(|index| {
            let result = search(&index, "");
            assert_eq!(result.0.len(), 3);
        });
    }

    #[tokio::test]
    async fn test_dependency() {
        assert_search(|index| {
            let result = search(&index, "dependency:openssl");
            assert_eq!(result.0.len(), 1);
        });
    }

    #[tokio::test]
    async fn test_not() {
        assert_search(|index| {
            let result = search(&index, "type:oci");
            assert_eq!(result.0.len(), 1);
            let result = search(&index, "NOT type:oci");
            assert_eq!(result.0.len(), 2);

            let result = search(&index, "NOT ubi9");
            assert_eq!(result.0.len(), 2);

            let result = search(&index, "type:oci NOT ubi9");
            assert_eq!(result.0.len(), 0);

            let result = search(&index, "type:oci NOT ubi8");
            assert_eq!(result.0.len(), 1);
        });
    }

    #[tokio::test]
    async fn test_quarkus() {
        assert_search(|index| {
            let result = search(&index, "dependency:quarkus-arc");
            assert_eq!(result.0.len(), 1);
        });
    }

    #[tokio::test]
    async fn test_sorting() {
        assert_search(|index| {
            let result = search(&index, "NOT ubi9 sort:created");
            assert_eq!(result.0.len(), 2);
            assert_eq!(result.0[0].document.id, "my-sbom");
            assert_eq!(result.0[1].document.id, "kmm-1");
            assert!(result.0[0].document.created < result.0[1].document.created);

            let result = search(&index, "NOT ubi9 -sort:created");
            assert_eq!(result.0.len(), 2);
            assert_eq!(result.0[0].document.id, "kmm-1");
            assert_eq!(result.0[1].document.id, "my-sbom");
            assert!(result.0[0].document.created > result.0[1].document.created);
        });
    }

    #[tokio::test]
    async fn test_purl_qualifiers() {
        assert_search(|index| {
            let result = search(&index, "qualifier:tag:9.1.0-1782");
            assert_eq!(result.0.len(), 1);
        });
    }

    #[tokio::test]
    async fn test_supplier() {
        assert_search(|index| {
            let result = search(&index, "supplier:\"Organization: Red Hat\"");
            assert_eq!(result.0.len(), 2);
        });

        assert_search(|index| {
            let result = search(&index, "\"Red Hat\" in:supplier");
            assert_eq!(result.0.len(), 2);
        });
    }

    #[tokio::test]
    async fn test_explain() {
        assert_search(|index| {
            let result = index
                .search(
                    "dependency:openssl",
                    0,
                    10000,
                    SearchOptions {
                        explain: true,
                        metadata: false,
                    },
                )
                .unwrap();
            assert_eq!(result.0.len(), 1);
            assert!(result.0[0].explanation.is_some());
            println!(
                "Explanation: {}",
                serde_json::to_string_pretty(&result.0[0].explanation.as_ref().unwrap()).unwrap()
            );
        });
    }
}
