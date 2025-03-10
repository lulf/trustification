use std::{net::TcpListener, sync::Arc};

use actix_cors::Cors;
use actix_web::{http::header::ContentType, web, HttpResponse, HttpServer};
use actix_web_prom::PrometheusMetricsBuilder;
use anyhow::anyhow;
use derive_more::{Display, Error, From};
use http::StatusCode;
use prometheus::Registry;
use spog_model::search;
use trustification_api::{search::SearchOptions, Apply};
use trustification_auth::{
    authenticator::Authenticator,
    authorizer::Authorizer,
    client::{Error as AuthClientError, TokenInjector, TokenProvider},
    swagger_ui::SwaggerUiOidc,
};
use trustification_common::error::ErrorInformation;
use trustification_infrastructure::app::{new_app, AppOptions};
use trustification_version::version;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::analyze::CrdaClient;
use crate::{advisory, analyze, config, index, sbom, Run};

pub struct Server {
    run: Run,
}

#[derive(OpenApi)]
#[openapi(
        paths(
            sbom::get,
            sbom::search,
            advisory::get,
            advisory::search,
            trustification_version::version::version_fn,
            //vulnerability::search,
        ),
        components(
            //schemas(search::PackageSummary, search::VulnSummary, search::SearchResult<Vec<search::PackageSummary>>)
            schemas(
                search::PackageSummary,
                search::SearchResult<Vec<search::PackageSummary>>,
                trustification_version::VersionInformation,
                trustification_version::Version,
                trustification_version::Git,
                trustification_version::Build,
            )
        ),
        tags(
            (name = "package", description = "Package endpoints"),
            (name = "advisory", description = "Advisory endpoints"),
          //  (name = "vulnerability", description = "Vulnerability endpoints"),
        ),
    )]
pub struct ApiDoc;

impl Server {
    pub fn new(run: Run) -> Self {
        Self { run }
    }

    pub async fn run(self, registry: &Registry, listener: Option<TcpListener>) -> anyhow::Result<()> {
        let state = configure(&self.run)?;

        let http_metrics = PrometheusMetricsBuilder::new("spog_api")
            .registry(registry.clone())
            .build()
            .map_err(|_| anyhow!("Error registering HTTP metrics"))?;

        let config_configurator = config::configurator(self.run.config).await?;

        let authorizer = if self.run.oidc.disabled {
            Authorizer::Disabled
        } else {
            Authorizer::Enabled
        };
        let authenticator: Option<Arc<Authenticator>> =
            Authenticator::from_devmode_or_config(self.run.devmode, self.run.oidc)
                .await?
                .map(Arc::new);

        let swagger_oidc: Option<Arc<SwaggerUiOidc>> =
            SwaggerUiOidc::from_devmode_or_config(self.run.devmode, self.run.swagger_ui_oidc)
                .await?
                .map(Arc::new);

        if authenticator.is_none() {
            log::warn!("Authentication is disabled");
        }

        let crda = self.run.crda_url.map(CrdaClient::new);
        let crda_payload_limit = self.run.crda_payload_limit;

        let mut srv = HttpServer::new(move || {
            let state = state.clone();

            let http_metrics = http_metrics.clone();
            let cors = Cors::permissive();
            let authenticator = authenticator.clone();
            let swagger_oidc = swagger_oidc.clone();

            let mut app = new_app(AppOptions {
                cors: Some(cors),
                metrics: Some(http_metrics),
                authenticator: None, // we map this explicitly
                authorizer,
            })
            .app_data(web::Data::new(state))
            .configure(index::configure())
            .configure(version::configurator(version!()))
            .configure(sbom::configure(authenticator.clone()))
            .configure(advisory::configure(authenticator.clone()))
            .configure(config_configurator.clone())
            .service({
                let mut openapi = ApiDoc::openapi();
                let mut swagger = SwaggerUi::new("/swagger-ui/{_:.*}");

                if let Some(swagger_ui_oidc) = &swagger_oidc {
                    swagger = swagger_ui_oidc.apply(swagger, &mut openapi);
                }

                swagger.url("/openapi.json", openapi)
            });

            if let Some(crda) = &crda {
                app = app
                    .app_data(web::Data::new(crda.clone()))
                    .configure(analyze::configure(crda_payload_limit));
            }

            app
        });
        srv = match listener {
            Some(v) => srv.listen(v)?,
            None => srv.bind((self.run.bind, self.run.port))?,
        };
        srv.run().await?;
        Ok(())
    }
}

#[derive(Debug, Display, Error, From)]
pub enum Error {
    #[display(fmt = "response error: {:?}", "_0")]
    Response(StatusCode, String),
    #[display(fmt = "request error: {:?}", "_0")]
    Request(reqwest::Error),
    #[display(fmt = "url parse error: {}", "_0")]
    UrlParse(url::ParseError),
    #[display(fmt = "authentication error: {}", "_0")]
    AuthClient(AuthClientError),
}

impl actix_web::error::ResponseError for Error {
    fn error_response(&self) -> HttpResponse {
        let mut res = HttpResponse::build(self.status_code());
        res.insert_header(ContentType::json());
        match self {
            Self::Response(status, error) => res.json(ErrorInformation {
                error: format!("{}", status),
                message: "Error response from backend service".to_string(),
                details: error.to_string(),
            }),
            Self::Request(error) => res.json(ErrorInformation {
                error: format!("{}", self.status_code()),
                message: "Error creating request to backend service".to_string(),
                details: error.to_string(),
            }),
            Self::UrlParse(error) => res.json(ErrorInformation {
                error: format!("{}", self.status_code()),
                message: "Error constructing url to backend service".to_string(),
                details: error.to_string(),
            }),
            Self::AuthClient(error) => res.json(ErrorInformation {
                error: format!("{}", self.status_code()),
                message: "Error creating authentication client".to_string(),
                details: error.to_string(),
            }),
        }
    }
    fn status_code(&self) -> StatusCode {
        match self {
            Self::Response(status, _) => *status,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

pub struct AppState {
    client: reqwest::Client,
    pub bombastic: reqwest::Url,
    pub vexination: reqwest::Url,
}

impl AppState {
    pub async fn get_sbom(
        &self,
        id: &str,
        provider: &dyn TokenProvider,
    ) -> Result<impl futures::Stream<Item = reqwest::Result<bytes::Bytes>>, Error> {
        let url = self.bombastic.join("/api/v1/sbom")?;
        let response = self
            .client
            .get(url)
            .query(&[("id", id)])
            .inject_token(provider)
            .await?
            .send()
            .await?;
        if response.status() == StatusCode::OK {
            Ok(response.bytes_stream())
        } else {
            let status = response.status();
            match response.text().await {
                Ok(body) => Err(Error::Response(status, body)),
                Err(e) => Err(Error::Request(e)),
            }
        }
    }

    pub async fn search_sbom(
        &self,
        q: &str,
        offset: usize,
        limit: usize,
        options: SearchOptions,
        provider: &dyn TokenProvider,
    ) -> Result<bombastic_model::search::SearchResult, Error> {
        let url = self.bombastic.join("/api/v1/sbom/search")?;
        let response = self
            .client
            .get(url)
            .query(&[("q", q)])
            .query(&[("offset", offset), ("limit", limit)])
            .apply(&options)
            .inject_token(provider)
            .await?
            .send()
            .await?;
        if response.status() == StatusCode::OK {
            Ok(response.json::<bombastic_model::prelude::SearchResult>().await?)
        } else {
            let status = response.status();
            match response.text().await {
                Ok(body) => Err(Error::Response(status, body)),
                Err(e) => Err(Error::Request(e)),
            }
        }
    }

    pub async fn get_vex(
        &self,
        id: &str,
        provider: &dyn TokenProvider,
    ) -> Result<impl futures::Stream<Item = reqwest::Result<bytes::Bytes>>, Error> {
        let url = self.vexination.join("/api/v1/vex")?;
        let response = self
            .client
            .get(url)
            .query(&[("advisory", id)])
            .inject_token(provider)
            .await?
            .send()
            .await?;
        if response.status() == StatusCode::OK {
            Ok(response.bytes_stream())
        } else {
            let status = response.status();
            match response.text().await {
                Ok(body) => Err(Error::Response(status, body)),
                Err(e) => Err(Error::Request(e)),
            }
        }
    }

    pub async fn search_vex(
        &self,
        q: &str,
        offset: usize,
        limit: usize,
        options: SearchOptions,
        provider: &dyn TokenProvider,
    ) -> Result<vexination_model::search::SearchResult, Error> {
        let url = self.vexination.join("/api/v1/vex/search")?;
        let response = self
            .client
            .get(url)
            .query(&[("q", q)])
            .query(&[("offset", offset), ("limit", limit)])
            .apply(&options)
            .inject_token(provider)
            .await?
            .send()
            .await?;
        if response.status() == StatusCode::OK {
            Ok(response.json::<vexination_model::prelude::SearchResult>().await?)
        } else {
            let status = response.status();
            match response.text().await {
                Ok(body) => Err(Error::Response(status, body)),
                Err(e) => Err(Error::Request(e)),
            }
        }
    }
}

pub type SharedState = Arc<AppState>;

pub(crate) fn configure(run: &Run) -> anyhow::Result<Arc<AppState>> {
    let state = Arc::new(AppState {
        client: reqwest::Client::new(),
        bombastic: run.bombastic_url.clone(),
        vexination: run.vexination_url.clone(),
    });
    Ok(state)
}
