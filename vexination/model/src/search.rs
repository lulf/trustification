use std::collections::HashMap;

use serde_json::Value;
use sikula::prelude::*;
use utoipa::ToSchema;

#[derive(Clone, Debug, PartialEq, Search)]
pub enum Vulnerabilities<'a> {
    #[search(default)]
    Id(&'a str),
    #[search(default)]
    Cve(&'a str),
    #[search(default)]
    Title(Primary<'a>),
    #[search(default)]
    Description(Primary<'a>),
    Status(&'a str),
    #[search(sort)]
    Severity(&'a str),
    Cvss(PartialOrdered<f64>),
    #[search(scope)]
    Package(Primary<'a>),
    #[search(scope)]
    Fixed(Primary<'a>),
    #[search(scope)]
    Affected(Primary<'a>),
    #[search]
    Initial(Ordered<time::OffsetDateTime>),
    #[search(sort)]
    Release(Ordered<time::OffsetDateTime>),
    #[search]
    Discovery(Ordered<time::OffsetDateTime>),
    Final,
    Critical,
    High,
    Medium,
    Low,
}

/// A document returned from the search index for every match.
#[derive(serde::Deserialize, serde::Serialize, Debug, PartialEq, ToSchema)]
pub struct SearchDocument {
    /// Advisory identifier
    pub advisory_id: String,
    /// Advisory title
    pub advisory_title: String,
    /// Advisory release date in RFC3339 format
    #[schema(value_type = String)]
    pub advisory_date: time::OffsetDateTime,
    /// Snippet highlighting part of description that matched
    pub advisory_snippet: String,
    /// Advisory description
    pub advisory_desc: String,
    /// Advisory severity
    pub advisory_severity: String,
    /// List of CVE identifiers that matched within the advisory
    pub cves: Vec<String>,
    /// Highest CVSS score in vulnerabilities matched within the advisory
    pub cvss_max: Option<f64>,
    /// Number of severities by level
    pub cve_severity_count: HashMap<String, u64>,
}

/// The hit describes the document, its score and optionally an explanation of why that score was given.
#[derive(serde::Deserialize, serde::Serialize, Debug, PartialEq, utoipa::ToSchema)]
pub struct SearchHit {
    /// The document that was matched.
    pub document: SearchDocument,
    /// Score as evaluated by the search engine.
    pub score: f32,
    /// Explanation of the score if enabled,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<Value>,
    /// Additional metadata, if enabled
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "$metadata")]
    pub metadata: Option<Value>,
}

/// The payload returned describing how many results matched and the matching documents (within offset and limit requested).
#[derive(serde::Deserialize, serde::Serialize, Debug, PartialEq, ToSchema)]
pub struct SearchResult {
    /// Total number of matching documents
    pub total: usize,
    /// Documents matched up to max requested
    pub result: Vec<SearchHit>,
}
