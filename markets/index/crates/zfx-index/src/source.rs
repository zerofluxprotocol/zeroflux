//! Parsers for OpenRouter responses. Every parser validates shape strictly and
//! returns `SourceError` instead of guessing: a renamed field, a float where an
//! integer token count belongs, a duplicate row or a non-midnight day label all
//! reject the whole blob. Nothing here uses floating point.

use crate::fixed::{parse_decimal, PRICE_SCALE};
use crate::time::{parse_http_date_ms, parse_rfc3339_ms, UtcDate};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Kinds of source responses the ingestor understands. `documented` marks the
/// endpoints described in OpenRouter's public API reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// GET /api/frontend/v1/rankings/models?view=day (public web endpoint)
    RankingsDay,
    /// GET /api/frontend/v1/rankings/models?view=day&metric=spend (public web endpoint)
    RankingsDaySpend,
    /// GET /api/frontend/v1/stats/model-activity?permaslug=..&variant=standard (public web endpoint)
    ModelActivity,
    /// GET /api/frontend/v1/stats/effective-pricing?permaslug=..&variant=standard&shape=v7&range=all (public web endpoint)
    EffectivePricing,
    /// GET /api/v1/models (documented, no auth)
    CatalogModels,
    /// GET /api/v1/models/{author}/{slug}/endpoints (documented, no auth)
    ModelEndpoints,
    /// GET /api/v1/datasets/rankings-daily (documented, requires an API key)
    RankingsDailyApi,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::RankingsDay => "rankings_day",
            Kind::RankingsDaySpend => "rankings_day_spend",
            Kind::ModelActivity => "model_activity",
            Kind::EffectivePricing => "effective_pricing",
            Kind::CatalogModels => "catalog_models",
            Kind::ModelEndpoints => "model_endpoints",
            Kind::RankingsDailyApi => "rankings_daily_api",
        }
    }
    pub fn from_name(s: &str) -> Option<Kind> {
        Some(match s {
            "rankings_day" => Kind::RankingsDay,
            "rankings_day_spend" => Kind::RankingsDaySpend,
            "model_activity" => Kind::ModelActivity,
            "effective_pricing" => Kind::EffectivePricing,
            "catalog_models" => Kind::CatalogModels,
            "model_endpoints" => Kind::ModelEndpoints,
            "rankings_daily_api" => Kind::RankingsDailyApi,
            _ => return None,
        })
    }
    pub fn documented(self) -> bool {
        matches!(self, Kind::CatalogModels | Kind::ModelEndpoints | Kind::RankingsDailyApi)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceError {
    NotJson(String),
    /// A required field is missing or has the wrong JSON type.
    Shape(String),
    /// A numeric field could not be read exactly.
    Number(String),
    /// Two rows share the same key.
    Duplicate(String),
    /// A day label is not a whole UTC day, or dates are inconsistent.
    BadDate(String),
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceError::NotJson(s) => write!(f, "NOT_JSON: {s}"),
            SourceError::Shape(s) => write!(f, "SHAPE_CHANGED: {s}"),
            SourceError::Number(s) => write!(f, "BAD_NUMBER: {s}"),
            SourceError::Duplicate(s) => write!(f, "DUPLICATE_ROW: {s}"),
            SourceError::BadDate(s) => write!(f, "BAD_DATE: {s}"),
        }
    }
}

type R<T> = Result<T, SourceError>;

pub fn parse_json(bytes: &[u8]) -> R<Value> {
    serde_json::from_slice(bytes).map_err(|e| SourceError::NotJson(e.to_string()))
}

fn field<'a>(v: &'a Value, k: &str, ctx: &str) -> R<&'a Value> {
    v.get(k)
        .ok_or_else(|| SourceError::Shape(format!("{ctx}: missing `{k}`")))
}
fn obj_field<'a>(v: &'a Value, k: &str, ctx: &str) -> R<&'a serde_json::Map<String, Value>> {
    field(v, k, ctx)?
        .as_object()
        .ok_or_else(|| SourceError::Shape(format!("{ctx}: `{k}` is not an object")))
}
fn arr_field<'a>(v: &'a Value, k: &str, ctx: &str) -> R<&'a Vec<Value>> {
    field(v, k, ctx)?
        .as_array()
        .ok_or_else(|| SourceError::Shape(format!("{ctx}: `{k}` is not an array")))
}
fn str_field<'a>(v: &'a Value, k: &str, ctx: &str) -> R<&'a str> {
    field(v, k, ctx)?
        .as_str()
        .ok_or_else(|| SourceError::Shape(format!("{ctx}: `{k}` is not a string")))
}
fn opt_str<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(|x| x.as_str()).filter(|s| !s.is_empty())
}

/// Literal decimal text of a JSON number (exact thanks to `arbitrary_precision`).
fn num_text(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Non-negative integer count (tokens, requests). Rejects fractions.
fn count(v: &Value, k: &str, ctx: &str) -> R<u128> {
    let x = field(v, k, ctx)?;
    let t = match x {
        Value::Number(_) => num_text(x).unwrap(),
        Value::String(s) => s.clone(),
        _ => return Err(SourceError::Shape(format!("{ctx}: `{k}` is not a number"))),
    };
    if !t.bytes().all(|c| c.is_ascii_digit()) || t.is_empty() {
        return Err(SourceError::Number(format!("{ctx}: `{k}` = {t} is not a non-negative integer")));
    }
    t.parse::<u128>()
        .map_err(|_| SourceError::Number(format!("{ctx}: `{k}` overflow")))
}

/// Price in USD per million tokens -> nano-USD per M.
fn price(v: &Value, ctx: &str) -> R<u128> {
    let t = num_text(v).ok_or_else(|| SourceError::Shape(format!("{ctx}: price is not a number")))?;
    parse_decimal(&t, PRICE_SCALE).map_err(|e| SourceError::Number(format!("{ctx}: {t}: {e:?}")))
}

fn day_label(s: &str, ctx: &str) -> R<UtcDate> {
    UtcDate::parse_day_label(s).ok_or_else(|| SourceError::BadDate(format!("{ctx}: `{s}` is not a whole UTC day label")))
}

// ---------------------------------------------------------------------------
// Usage rows (model-activity and rankings/models share the row schema)

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageRow {
    pub date: UtcDate,
    pub permaslug: String,
    pub variant: String,
    pub prompt_tokens: u128,
    pub completion_tokens: u128,
    pub reasoning_tokens: u128,
    pub cached_tokens: u128,
    pub requests: u128,
    pub byok_prompt_tokens: u128,
    pub byok_completion_tokens: u128,
}

impl UsageRow {
    pub fn total_tokens(&self) -> u128 {
        self.prompt_tokens + self.completion_tokens
    }
}

fn usage_row(r: &Value, ctx: &str) -> R<UsageRow> {
    let date = day_label(str_field(r, "date", ctx)?, ctx)?;
    Ok(UsageRow {
        date,
        permaslug: str_field(r, "model_permaslug", ctx)?.to_string(),
        variant: str_field(r, "variant", ctx)?.to_string(),
        prompt_tokens: count(r, "total_prompt_tokens", ctx)?,
        completion_tokens: count(r, "total_completion_tokens", ctx)?,
        reasoning_tokens: count(r, "total_native_tokens_reasoning", ctx)?,
        cached_tokens: count(r, "total_native_tokens_cached", ctx)?,
        requests: count(r, "count", ctx)?,
        byok_prompt_tokens: count(r, "total_byok_prompt_tokens", ctx)?,
        byok_completion_tokens: count(r, "total_byok_completion_tokens", ctx)?,
    })
}

#[derive(Clone, Debug)]
pub struct ActivityDoc {
    pub rows: Vec<UsageRow>,
    /// Server-side computation time (`cachedAt`, unix ms). The CDN may serve the
    /// response for hours, so this, not the fetch time, says how fresh the data is.
    pub cached_at_ms: Option<i64>,
}

pub fn parse_model_activity(bytes: &[u8]) -> R<ActivityDoc> {
    let v = parse_json(bytes)?;
    let data = field(&v, "data", "model-activity")?;
    let a = arr_field(data, "analytics", "model-activity.data")?;
    let mut rows = Vec::with_capacity(a.len());
    let mut seen = BTreeSet::new();
    for (i, r) in a.iter().enumerate() {
        let row = usage_row(r, &format!("model-activity.analytics[{i}]"))?;
        if !seen.insert((row.date, row.permaslug.clone(), row.variant.clone())) {
            return Err(SourceError::Duplicate(format!("{} {} {}", row.date, row.permaslug, row.variant)));
        }
        rows.push(row);
    }
    let cached_at_ms = data
        .get("cachedAt")
        .and_then(num_text)
        .and_then(|t| t.parse::<i64>().ok());
    Ok(ActivityDoc { rows, cached_at_ms })
}

#[derive(Clone, Debug)]
pub struct RankRow {
    pub usage: UsageRow,
    /// `rankingMetricValue` literal text (tokens, or a spend share in [0,1]).
    pub metric_text: String,
}

#[derive(Clone, Debug)]
pub struct RankingsDayDoc {
    pub date: UtcDate,
    pub rows: Vec<RankRow>,
}

/// `rankings/models?view=day[&metric=spend]`. All rows must carry the same date.
pub fn parse_rankings_day(bytes: &[u8]) -> R<RankingsDayDoc> {
    let v = parse_json(bytes)?;
    let a = arr_field(&v, "data", "rankings")?;
    if a.is_empty() {
        return Err(SourceError::Shape("rankings: empty data".into()));
    }
    let mut rows = Vec::with_capacity(a.len());
    let mut seen = BTreeSet::new();
    let mut date = None;
    for (i, r) in a.iter().enumerate() {
        let ctx = format!("rankings.data[{i}]");
        let u = usage_row(r, &ctx)?;
        match date {
            None => date = Some(u.date),
            Some(d) if d != u.date => {
                return Err(SourceError::BadDate(format!("{ctx}: mixed dates {d} and {}", u.date)))
            }
            _ => {}
        }
        if !seen.insert((u.permaslug.clone(), u.variant.clone())) {
            return Err(SourceError::Duplicate(format!("{} {}", u.permaslug, u.variant)));
        }
        let m = field(r, "rankingMetricValue", &ctx)?;
        let metric_text = num_text(m).ok_or_else(|| SourceError::Shape(format!("{ctx}: rankingMetricValue not a number")))?;
        rows.push(RankRow { usage: u, metric_text });
    }
    Ok(RankingsDayDoc { date: date.unwrap(), rows })
}

// ---------------------------------------------------------------------------
// Effective pricing

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSummary {
    pub endpoint_id: String,
    pub provider_slug: String,
    pub provider_name: String,
    pub eff_in: Option<u128>,
    pub eff_out: Option<u128>,
    pub total_tokens: u128,
}

#[derive(Clone, Debug)]
pub struct EffectivePricingDoc {
    /// Token-weighted effective prices over OpenRouter's trailing summary window
    /// (observed to be about 24 hours). nano-USD per M tokens.
    pub weighted_in: Option<u128>,
    pub weighted_out: Option<u128>,
    pub providers: Vec<ProviderSummary>,
    /// Per-UTC-day, per-endpoint effective prices. nano-USD per M tokens.
    pub input_by_day: BTreeMap<UtcDate, BTreeMap<String, u128>>,
    pub output_by_day: BTreeMap<UtcDate, BTreeMap<String, u128>>,
}

fn chart(v: &Value, k: &str) -> R<BTreeMap<UtcDate, BTreeMap<String, u128>>> {
    let mut out = BTreeMap::new();
    let a = arr_field(v, k, "effective-pricing")?;
    for (i, p) in a.iter().enumerate() {
        let ctx = format!("effective-pricing.{k}[{i}]");
        let d = day_label(str_field(p, "x", &ctx)?, &ctx)?;
        let y = obj_field(p, "y", &ctx)?;
        let mut m = BTreeMap::new();
        for (ep, val) in y {
            if val.is_null() {
                continue;
            }
            m.insert(ep.clone(), price(val, &format!("{ctx}.y.{ep}"))?);
        }
        if out.insert(d, m).is_some() {
            return Err(SourceError::Duplicate(format!("{ctx}: day {d} twice")));
        }
    }
    Ok(out)
}

pub fn parse_effective_pricing(bytes: &[u8]) -> R<EffectivePricingDoc> {
    let v = parse_json(bytes)?;
    let d = field(&v, "data", "effective-pricing")?;
    let opt_price = |k: &str| -> R<Option<u128>> {
        match d.get(k) {
            None => Err(SourceError::Shape(format!("effective-pricing: missing `{k}`"))),
            Some(Value::Null) => Ok(None),
            Some(x) => price(x, k).map(Some),
        }
    };
    let weighted_in = opt_price("weightedInputPrice")?;
    let weighted_out = opt_price("weightedOutputPrice")?;
    let mut providers = Vec::new();
    let mut seen = BTreeSet::new();
    for (i, p) in arr_field(d, "providerSummaries", "effective-pricing")?.iter().enumerate() {
        let ctx = format!("effective-pricing.providerSummaries[{i}]");
        let endpoint_id = str_field(p, "endpointId", &ctx)?.to_string();
        if !seen.insert(endpoint_id.clone()) {
            return Err(SourceError::Duplicate(format!("{ctx}: endpoint {endpoint_id}")));
        }
        let op = |k: &str| -> R<Option<u128>> {
            match p.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(x) => price(x, &format!("{ctx}.{k}")).map(Some),
            }
        };
        providers.push(ProviderSummary {
            provider_slug: str_field(p, "providerSlug", &ctx)?.to_string(),
            provider_name: str_field(p, "providerName", &ctx)?.to_string(),
            eff_in: op("effectiveInputPrice")?,
            eff_out: op("effectiveOutputPrice")?,
            total_tokens: count(p, "totalTokens", &ctx)?,
            endpoint_id,
        });
    }
    Ok(EffectivePricingDoc {
        weighted_in,
        weighted_out,
        providers,
        input_by_day: chart(d, "inputChartData")?,
        output_by_day: chart(d, "outputChartData")?,
    })
}

// ---------------------------------------------------------------------------
// Documented catalog endpoints

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogModel {
    pub id: String,
    pub canonical_slug: String,
    pub name: String,
    pub author: String,
    pub hugging_face_id: Option<String>,
    pub created_s: Option<i64>,
    pub modality: Option<String>,
    /// Listed per-token prices converted to nano-USD per M tokens.
    pub listed_in: Option<u128>,
    pub listed_out: Option<u128>,
}

fn listed(p: &Value, k: &str) -> Option<u128> {
    // per-token USD strings, e.g. "0.000004"; 15 decimals of per-token = 9 decimals per M
    let s = p.get(k)?.as_str()?;
    parse_decimal(s, PRICE_SCALE + 6).ok()
}

pub fn parse_catalog(bytes: &[u8]) -> R<Vec<CatalogModel>> {
    let v = parse_json(bytes)?;
    let a = arr_field(&v, "data", "models")?;
    let mut out = Vec::with_capacity(a.len());
    let mut seen = BTreeSet::new();
    for (i, m) in a.iter().enumerate() {
        let ctx = format!("models.data[{i}]");
        let id = str_field(m, "id", &ctx)?.to_string();
        if !seen.insert(id.clone()) {
            return Err(SourceError::Duplicate(format!("model id {id}")));
        }
        let canonical_slug = opt_str(m, "canonical_slug").unwrap_or(&id).to_string();
        let pricing = field(m, "pricing", &ctx)?;
        out.push(CatalogModel {
            author: id.split('/').next().unwrap_or("").to_string(),
            name: str_field(m, "name", &ctx)?.to_string(),
            hugging_face_id: opt_str(m, "hugging_face_id").map(String::from),
            created_s: m.get("created").and_then(num_text).and_then(|t| t.parse().ok()),
            modality: m
                .get("architecture")
                .and_then(|a| opt_str(a, "modality"))
                .map(String::from),
            listed_in: listed(pricing, "prompt"),
            listed_out: listed(pricing, "completion"),
            canonical_slug,
            id,
        });
    }
    Ok(out)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedEndpoint {
    pub provider_name: String,
    pub tag: Option<String>,
    pub listed_in: Option<u128>,
    pub listed_out: Option<u128>,
    pub listed_cache_read: Option<u128>,
}

pub fn parse_model_endpoints(bytes: &[u8]) -> R<Vec<ListedEndpoint>> {
    let v = parse_json(bytes)?;
    let d = field(&v, "data", "endpoints")?;
    let a = arr_field(d, "endpoints", "endpoints.data")?;
    let mut out = Vec::new();
    for (i, e) in a.iter().enumerate() {
        let ctx = format!("endpoints.data.endpoints[{i}]");
        let p = field(e, "pricing", &ctx)?;
        out.push(ListedEndpoint {
            provider_name: str_field(e, "provider_name", &ctx)?.to_string(),
            tag: opt_str(e, "tag").map(String::from),
            listed_in: listed(p, "prompt"),
            listed_out: listed(p, "completion"),
            listed_cache_read: listed(p, "input_cache_read"),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Documented keyed dataset: /api/v1/datasets/rankings-daily

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DailyRow {
    pub date: UtcDate,
    pub permaslug: String,
    pub total_tokens: u128,
}

#[derive(Clone, Debug)]
pub struct RankingsDailyDoc {
    pub rows: Vec<DailyRow>,
    pub as_of_ms: i64,
    pub start_date: Option<UtcDate>,
    pub end_date: UtcDate,
    pub version: String,
}

/// Accepts the wire format (snake_case, as in the spec) and the SDK's camelCase.
pub fn parse_rankings_daily_api(bytes: &[u8]) -> R<RankingsDailyDoc> {
    let v = parse_json(bytes)?;
    let meta = field(&v, "meta", "rankings-daily")?;
    let pick = |o: &Value, a: &str, b: &str| -> Option<Value> { o.get(a).or_else(|| o.get(b)).cloned() };
    let as_of = pick(meta, "as_of", "asOf")
        .and_then(|x| x.as_str().map(String::from))
        .ok_or_else(|| SourceError::Shape("rankings-daily.meta: missing as_of".into()))?;
    let as_of_ms = parse_rfc3339_ms(&as_of).ok_or_else(|| SourceError::BadDate(format!("as_of {as_of}")))?;
    let end_date = pick(meta, "end_date", "endDate")
        .and_then(|x| x.as_str().and_then(UtcDate::parse))
        .ok_or_else(|| SourceError::Shape("rankings-daily.meta: missing end_date".into()))?;
    let start_date = pick(meta, "start_date", "startDate").and_then(|x| x.as_str().and_then(UtcDate::parse));
    let version = pick(meta, "version", "version")
        .and_then(|x| x.as_str().map(String::from))
        .ok_or_else(|| SourceError::Shape("rankings-daily.meta: missing version".into()))?;
    let mut rows = Vec::new();
    let mut seen = BTreeSet::new();
    for (i, r) in arr_field(&v, "data", "rankings-daily")?.iter().enumerate() {
        let ctx = format!("rankings-daily.data[{i}]");
        let date = day_label(str_field(r, "date", &ctx)?, &ctx)?;
        let permaslug = pick(r, "model_permaslug", "modelPermaslug")
            .and_then(|x| x.as_str().map(String::from))
            .ok_or_else(|| SourceError::Shape(format!("{ctx}: missing model_permaslug")))?;
        let tt = pick(r, "total_tokens", "totalTokens")
            .ok_or_else(|| SourceError::Shape(format!("{ctx}: missing total_tokens")))?;
        let tt_text = match &tt {
            Value::String(s) => s.clone(),
            Value::Number(_) => num_text(&tt).unwrap(),
            _ => return Err(SourceError::Shape(format!("{ctx}: total_tokens type"))),
        };
        if tt_text.is_empty() || !tt_text.bytes().all(|c| c.is_ascii_digit()) {
            return Err(SourceError::Number(format!("{ctx}: total_tokens {tt_text}")));
        }
        if date > end_date {
            return Err(SourceError::BadDate(format!("{ctx}: row date {date} after meta.end_date {end_date}")));
        }
        if !seen.insert((date, permaslug.clone())) {
            return Err(SourceError::Duplicate(format!("{date} {permaslug}")));
        }
        rows.push(DailyRow {
            date,
            permaslug,
            total_tokens: tt_text.parse().map_err(|_| SourceError::Number(format!("{ctx}: overflow")))?,
        });
    }
    Ok(RankingsDailyDoc { rows, as_of_ms, start_date, end_date, version })
}

/// Server time a response was computed at: `Last-Modified` if present, else the
/// origin response time `Date - Age` (RFC 9111: Age is seconds since the origin
/// generated or revalidated the response). None if neither is available.
pub fn last_modified_ms(headers: &BTreeMap<String, String>) -> Option<i64> {
    if let Some(t) = headers.get("last-modified").and_then(|s| parse_http_date_ms(s)) {
        return Some(t);
    }
    let date = headers.get("date").and_then(|s| parse_http_date_ms(s))?;
    let age: i64 = headers.get("age").and_then(|a| a.trim().parse().ok()).unwrap_or(0);
    Some(date - age * 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACT: &str = r#"{"data":{"analytics":[
      {"date":"2026-11-10 00:00:00","model_permaslug":"a/m-1","variant":"standard","total_completion_tokens":5,"total_prompt_tokens":95,"total_native_tokens_reasoning":1,"count":3,"num_media_prompt":0,"total_native_tokens_cached":50,"total_tool_calls":0,"total_byok_prompt_tokens":10,"total_byok_completion_tokens":1,"variant_permaslug":"a/m-1"},
      {"date":"2026-11-09 00:00:00","model_permaslug":"a/m-1","variant":"standard","total_completion_tokens":10,"total_prompt_tokens":90,"total_native_tokens_reasoning":0,"count":4,"total_native_tokens_cached":0,"total_byok_prompt_tokens":0,"total_byok_completion_tokens":0}
    ],"cachedAt":1794340473165}}"#;

    #[test]
    fn activity_ok() {
        let d = parse_model_activity(ACT.as_bytes()).unwrap();
        assert_eq!(d.rows.len(), 2);
        assert_eq!(d.rows[0].total_tokens(), 100);
        assert_eq!(d.cached_at_ms, Some(1794340473165));
    }

    #[test]
    fn activity_malformed() {
        assert!(matches!(parse_model_activity(b"<html>"), Err(SourceError::NotJson(_))));
        assert!(matches!(parse_model_activity(br#"{"data":{}}"#), Err(SourceError::Shape(_))));
        let renamed = ACT.replace("total_prompt_tokens", "prompt_tokens");
        assert!(matches!(parse_model_activity(renamed.as_bytes()), Err(SourceError::Shape(_))));
        let float = ACT.replace("\"total_prompt_tokens\":95", "\"total_prompt_tokens\":95.5");
        assert!(matches!(parse_model_activity(float.as_bytes()), Err(SourceError::Number(_))));
        let neg = ACT.replace("\"count\":3", "\"count\":-3");
        assert!(matches!(parse_model_activity(neg.as_bytes()), Err(SourceError::Number(_))));
        let dup = ACT.replace("2026-11-10 00:00:00", "2026-11-09 00:00:00");
        assert!(matches!(parse_model_activity(dup.as_bytes()), Err(SourceError::Duplicate(_))));
        let hour = ACT.replace("2026-11-10 00:00:00", "2026-11-10 04:00:00");
        assert!(matches!(parse_model_activity(hour.as_bytes()), Err(SourceError::BadDate(_))));
        let strtype = ACT.replace("\"date\":\"2026-11-09 00:00:00\"", "\"date\":20261109");
        assert!(matches!(parse_model_activity(strtype.as_bytes()), Err(SourceError::Shape(_))));
    }

    #[test]
    fn effective_pricing_exact() {
        let b = br#"{"data":{"weightedInputPrice":0.5472722862297554,"weightedOutputPrice":10.024761587007943,"weightedCacheHitRate":0.8,
          "providerSummaries":[{"endpointId":"e1","providerName":"P1","providerSlug":"p1","effectiveInputPrice":0.56,"effectiveOutputPrice":10,"cacheHitRate":0.7,"totalTokens":135619852153}],
          "inputChartData":[{"x":"2026-11-09 00:00:00","y":{"e1":0.5,"e2":null}}],
          "outputChartData":[{"x":"2026-11-09 00:00:00","y":{"e1":9.9999300153817}}]}}"#;
        let d = parse_effective_pricing(b).unwrap();
        assert_eq!(d.weighted_in, Some(547_272_286));
        assert_eq!(d.weighted_out, Some(10_024_761_587));
        let day = UtcDate::parse("2026-11-09").unwrap();
        assert_eq!(d.input_by_day[&day]["e1"], 500_000_000);
        assert!(!d.input_by_day[&day].contains_key("e2"));
        assert_eq!(d.output_by_day[&day]["e1"], 9_999_930_015);
        let bad = std::str::from_utf8(b).unwrap().replace("\"effectiveOutputPrice\":10", "\"effectiveOutputPrice\":\"10\"");
        assert!(parse_effective_pricing(bad.as_bytes()).is_err());
    }

    #[test]
    fn rankings_day_mixed_dates_rejected() {
        let row = |d: &str, s: &str| format!(r#"{{"date":"{d}","model_permaslug":"{s}","variant":"standard","total_completion_tokens":1,"total_prompt_tokens":1,"total_native_tokens_reasoning":0,"count":1,"total_native_tokens_cached":0,"total_byok_prompt_tokens":0,"total_byok_completion_tokens":0,"rankingMetricValue":2}}"#);
        let ok = format!(r#"{{"data":[{},{}]}}"#, row("2026-11-09 00:00:00", "a/x"), row("2026-11-09 00:00:00", "a/y"));
        assert_eq!(parse_rankings_day(ok.as_bytes()).unwrap().rows.len(), 2);
        let mixed = format!(r#"{{"data":[{},{}]}}"#, row("2026-11-09 00:00:00", "a/x"), row("2026-11-08 00:00:00", "a/y"));
        assert!(matches!(parse_rankings_day(mixed.as_bytes()), Err(SourceError::BadDate(_))));
        let dup = format!(r#"{{"data":[{},{}]}}"#, row("2026-11-09 00:00:00", "a/x"), row("2026-11-09 00:00:00", "a/x"));
        assert!(matches!(parse_rankings_day(dup.as_bytes()), Err(SourceError::Duplicate(_))));
        assert!(parse_rankings_day(br#"{"data":[]}"#).is_err());
    }

    #[test]
    fn rankings_daily_api_shapes() {
        let snake = br#"{"data":[{"date":"2026-11-09","model_permaslug":"a/x","total_tokens":"123456789012345678901"},{"date":"2026-11-09","model_permaslug":"other","total_tokens":"5"}],
          "meta":{"as_of":"2026-11-10T02:00:00.000Z","start_date":"2026-11-01","end_date":"2026-11-09","version":"v1"}}"#;
        let d = parse_rankings_daily_api(snake).unwrap();
        assert_eq!(d.rows[0].total_tokens, 123456789012345678901);
        assert_eq!(d.version, "v1");
        let camel = br#"{"data":[{"date":"2026-11-09","modelPermaslug":"a/x","totalTokens":"7"}],"meta":{"asOf":"2026-11-10T02:00:00.000Z","endDate":"2026-11-09","version":"v1"}}"#;
        assert_eq!(parse_rankings_daily_api(camel).unwrap().rows[0].total_tokens, 7);
        let crossed = br#"{"data":[{"date":"2026-11-10","model_permaslug":"a/x","total_tokens":"7"}],"meta":{"as_of":"2026-11-10T02:00:00.000Z","end_date":"2026-11-09","version":"v1"}}"#;
        assert!(matches!(parse_rankings_daily_api(crossed), Err(SourceError::BadDate(_))));
        let frac = br#"{"data":[{"date":"2026-11-09","model_permaslug":"a/x","total_tokens":"7.5"}],"meta":{"as_of":"2026-11-10T02:00:00.000Z","end_date":"2026-11-09","version":"v1"}}"#;
        assert!(matches!(parse_rankings_daily_api(frac), Err(SourceError::Number(_))));
    }

    #[test]
    fn source_time_from_headers() {
        let mut h = BTreeMap::new();
        h.insert("date".to_string(), "Tue, 10 Nov 2026 23:11:58 GMT".to_string());
        h.insert("age".to_string(), "61".to_string());
        assert_eq!(crate::time::fmt_ms(last_modified_ms(&h).unwrap()), "2026-11-10T23:10:57.000Z");
        h.insert("last-modified".to_string(), "Tue, 10 Nov 2026 21:43:15 GMT".to_string());
        assert_eq!(crate::time::fmt_ms(last_modified_ms(&h).unwrap()), "2026-11-10T21:43:15.000Z");
        assert_eq!(last_modified_ms(&BTreeMap::new()), None);
    }

    #[test]
    fn catalog_listed_prices() {
        let b = br#"{"data":[{"id":"a/x","canonical_slug":"a/x-2026","name":"A: X","hugging_face_id":"","created":1,"architecture":{"modality":"text->text"},"pricing":{"prompt":"0.000004","completion":"0.00002"}}]}"#;
        let c = parse_catalog(b).unwrap();
        assert_eq!(c[0].listed_in, Some(4_000_000_000));
        assert_eq!(c[0].listed_out, Some(20_000_000_000));
        assert_eq!(c[0].hugging_face_id, None);
        assert_eq!(c[0].author, "a");
    }
}
