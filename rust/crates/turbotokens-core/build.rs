use std::{env, fs, path::PathBuf};

use serde_json::{Map, Value};

#[cfg(feature = "fetch-litellm-pricing")]
const LITELLM_PRICING_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
const OUT_PRICING_JSON: &str = "litellm-pricing.json";
const PRICING_JSON_PATH_ENV: &str = "TURBOTOKENS_PRICING_JSON_PATH";
#[cfg(feature = "fetch-litellm-pricing")]
const PRICING_FETCH_TIMEOUT_SECONDS: u64 = 10;

fn main() {
    println!("cargo:rerun-if-env-changed={PRICING_JSON_PATH_ENV}");

    let out_path = out_dir_path(OUT_PRICING_JSON);
    let pricing_json = if let Some(path) = env::var_os(PRICING_JSON_PATH_ENV) {
        let path = PathBuf::from(path);
        println!("cargo:rerun-if-changed={}", path.display());
        fs::read_to_string(path).expect("read pricing snapshot from TURBOTOKENS_PRICING_JSON_PATH")
    } else {
        fetch_pricing_json()
    };
    let pricing_json = compact_pricing_json(&pricing_json).expect("compact LiteLLM pricing JSON");

    fs::write(out_path, pricing_json).expect("write build-time pricing snapshot");
}

fn out_dir_path(file_name: &str) -> PathBuf {
    PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by cargo")).join(file_name)
}

// The snapshot can be handed to the build by TURBOTOKENS_PRICING_JSON_PATH for
// network-free builds. Downloading it is behind a feature so the HTTPS client
// (rustls and its C crypto backend, the single most expensive build-dependency
// in the workspace) stays off the default build's critical path.
#[cfg(not(feature = "fetch-litellm-pricing"))]
fn fetch_pricing_json() -> String {
    panic!(
        "no LiteLLM pricing snapshot available: set {PRICING_JSON_PATH_ENV} to a \
         model_prices_and_context_window.json, or build with \
         --features fetch-litellm-pricing to download it"
    );
}

#[cfg(feature = "fetch-litellm-pricing")]
fn fetch_pricing_json() -> String {
    download_pricing_json().expect("fetch LiteLLM pricing for embed")
}

#[cfg(feature = "fetch-litellm-pricing")]
fn download_pricing_json() -> std::io::Result<String> {
    let response = minreq::get(LITELLM_PRICING_URL)
        .with_timeout(PRICING_FETCH_TIMEOUT_SECONDS)
        .send()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    if response.status_code != 200 {
        return Err(std::io::Error::other(format!(
            "HTTP {}",
            response.status_code
        )));
    }
    Ok(response
        .as_str()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))?
        .to_string())
}

fn compact_pricing_json(json: &str) -> Option<String> {
    let Value::Object(raw) = serde_json::from_str::<Value>(json).ok()? else {
        return None;
    };
    let mut compact = Map::new();
    for (model, pricing) in raw {
        if !is_embedded_model(&model) {
            continue;
        }
        let Value::Object(pricing) = pricing else {
            continue;
        };
        let mut fields = Map::new();
        for (source, target) in [
            ("input_cost_per_token", "i"),
            ("output_cost_per_token", "o"),
            ("cache_creation_input_token_cost", "cc"),
            ("cache_read_input_token_cost", "cr"),
            ("input_cost_per_token_above_200k_tokens", "ia"),
            ("output_cost_per_token_above_200k_tokens", "oa"),
            ("cache_creation_input_token_cost_above_200k_tokens", "cca"),
            ("cache_read_input_token_cost_above_200k_tokens", "cra"),
            ("max_input_tokens", "ctx"),
        ] {
            let Some(value) = pricing.get(source) else {
                continue;
            };
            if !value.is_null() {
                fields.insert(target.to_string(), value.clone());
            }
        }
        if let Some(fast) = pricing
            .get("provider_specific_entry")
            .and_then(Value::as_object)
            .and_then(|entry| entry.get("fast"))
            .filter(|value| !value.is_null())
        {
            fields.insert("fast".to_string(), fast.clone());
        }
        if fields.contains_key("i") && fields.contains_key("o") {
            compact.insert(model, Value::Object(fields));
        }
    }
    serde_json::to_string(&Value::Object(compact)).ok()
}

fn is_embedded_model(model: &str) -> bool {
    model.starts_with("claude-")
        || model.starts_with("anthropic.")
        || model.starts_with("anthropic/")
        || model.starts_with("us.anthropic.")
        || model.starts_with("eu.anthropic.")
        || model.starts_with("global.anthropic.")
        || model.starts_with("jp.anthropic.")
        || model.starts_with("au.anthropic.")
        || model.starts_with("gpt-")
        || model.starts_with("openai/")
        || model.starts_with("azure/")
        || model.starts_with("zai/")
        || model.starts_with("openrouter/openai/")
        // Meta's published Muse API rates; the Muse adapter prices only exact
        // `meta/<model>` entries, so offline reports can show Muse cost.
        || model.starts_with("meta/muse-")
}
