//! The web module through Emory's MCP server, and its HTTP client, against a server of the tests
//! on the loopback that serves recorded answers of Wikipedia and Open-Meteo
//! (`tests/fixtures/web/`). Nothing reaches the internet, except the tests marked `#[ignore]`, run
//! by hand.

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Router,
    extract::{Request, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use mind::{
    tools::{self, Module, Tools},
    web::{Fetch, FetchError, Http, Web, WebConfig},
};
use rmcp::{
    ServiceExt,
    model::CallToolRequestParams,
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::{Value, json};

const SEARCH_EIFFEL: &str = include_str!("fixtures/web/search-fr-tour-eiffel.json");
const SUMMARY_EIFFEL: &str = include_str!("fixtures/web/summary-fr-tour-eiffel.json");
const GEOCODE_VALENCE: &str = include_str!("fixtures/web/weather-geocode-valence.json");
const GEOCODE_UNKNOWN: &str = include_str!("fixtures/web/weather-geocode-unknown.json");
const FORECAST_VALENCE: &str = include_str!("fixtures/web/weather-forecast-valence.json");

/// What the fake Wikipedia was asked: the path and query, and the headers that matter.
#[derive(Default)]
struct Seen(Mutex<Vec<Asked>>);

/// The address asked, the `User-Agent` and the `Accept` of a request.
type Asked = (String, Option<String>, Option<String>);

/// The fake Wikipedia and Open-Meteo: the Eiffel Tower, a search that never ends, an answer too
/// heavy, a nonsense subject with nothing; the weather of Valence, and a place nobody knows.
async fn wikipedia(State(seen): State<Arc<Seen>>, request: Request) -> Response {
    let headers: &HeaderMap = request.headers();
    let header = |name| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let uri = request.uri().to_string();
    seen.0.lock().unwrap().push((
        uri.clone(),
        header(header::USER_AGENT),
        header(header::ACCEPT),
    ));
    let json = |body: String| ([(header::CONTENT_TYPE, "application/json")], body).into_response();
    match request.uri().path() {
        "/w/api.php" if uri.contains("srsearch=tour+eiffel") => json(SEARCH_EIFFEL.to_owned()),
        "/w/api.php" if uri.contains("srsearch=lent") => {
            tokio::time::sleep(Duration::from_secs(5)).await;
            json(SEARCH_EIFFEL.to_owned())
        }
        "/w/api.php" => json(
            r#"{"batchcomplete":true,"query":{"searchinfo":{"totalhits":0},"search":[]}}"#
                .to_owned(),
        ),
        "/api/rest_v1/page/summary/Tour_Eiffel" => json(SUMMARY_EIFFEL.to_owned()),
        "/v1/search" if uri.contains("name=Valence%2C+France") => json(GEOCODE_VALENCE.to_owned()),
        "/v1/search" if uri.contains("name=lent") => {
            tokio::time::sleep(Duration::from_secs(5)).await;
            json(GEOCODE_VALENCE.to_owned())
        }
        "/v1/search" => json(GEOCODE_UNKNOWN.to_owned()),
        "/v1/forecast" if uri.contains("latitude=44.9256&longitude=4.90956&") => {
            json(FORECAST_VALENCE.to_owned())
        }
        "/v1/forecast" => (StatusCode::BAD_REQUEST, "{\"error\":true}").into_response(),
        "/heavy" => json("x".repeat(3000)),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn serve_fake() -> (SocketAddr, Arc<Seen>) {
    let seen = Arc::new(Seen::default());
    let app = Router::new().fallback(wikipedia).with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    (address, seen)
}

fn config(toml: &str) -> WebConfig {
    let config: WebConfig = toml::from_str(toml).unwrap();
    config.check().unwrap();
    config
}

/// The module, asking the fake instead of Wikipedia and Open-Meteo.
fn web_at(address: SocketAddr, config: &WebConfig) -> Web {
    let origin = format!("http://{address}");
    Web::new(config)
        .unwrap()
        .unwrap()
        .wikipedia_at(&origin)
        .weather_at(&origin, &origin)
}

fn said(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect()
}

#[tokio::test]
async fn the_tool_reaches_the_llm_through_emorys_server_and_answers() {
    let (address, seen) = serve_fake().await;
    let web = web_at(address, &config("user_agent = \"Emory/test (contact)\""));
    let server = tools::serve(Tools::new(vec![Arc::new(web)]), ([127, 0, 0, 1], 0).into())
        .await
        .unwrap();
    let client = ()
        .serve(StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(server.url.as_str())
                .auth_header(server.key.clone()),
        ))
        .await
        .unwrap();

    // What the LLM is offered: the weather and Wikipedia, with short descriptions.
    let offered = client.list_all_tools().await.unwrap();
    let mut names: Vec<_> = offered.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    assert_eq!(names, ["web_weather", "web_wikipedia"]);
    let tool = offered.iter().find(|t| t.name == "web_wikipedia").unwrap();
    let description = tool.description.as_deref().unwrap();
    assert!(description.contains("Wikipédia") && description.len() < 100);
    let schema = Value::Object((*tool.input_schema).clone());
    assert_eq!(schema["required"], json!(["query"]));
    assert_eq!(schema["properties"]["query"]["type"], "string");

    let mut ask = CallToolRequestParams::new("web_wikipedia");
    ask.arguments = json!({"query": "  tour   eiffel "}).as_object().cloned();
    let result = client.call_tool(ask).await.unwrap();
    assert_ne!(result.is_error, Some(true), "{}", said(&result));
    let answer: Value = serde_json::from_str(&said(&result)).unwrap();
    assert_eq!(answer["title"], "Tour Eiffel");
    assert_eq!(answer["lang"], "fr");
    assert!(!answer["extract"].as_str().unwrap().contains('['));

    // The requests carry the User-Agent of the configuration and ask for JSON.
    let seen = seen.0.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(
        seen[0]
            .0
            .starts_with("/w/api.php?action=query&list=search&srsearch=tour+eiffel&")
    );
    assert_eq!(seen[1].0, "/api/rest_v1/page/summary/Tour_Eiffel");
    for (_, agent, accept) in &seen {
        assert_eq!(agent.as_deref(), Some("Emory/test (contact)"));
        assert_eq!(accept.as_deref(), Some("application/json"));
    }

    // A subject nobody wrote about, in both editions: said simply, as a tool error.
    let mut nothing = CallToolRequestParams::new("web_wikipedia");
    nothing.arguments = json!({"query": "zzqxv"}).as_object().cloned();
    let result = client.call_tool(nothing).await.unwrap();
    assert_eq!(result.is_error, Some(true));
    assert_eq!(said(&result), "Wikipédia n'a pas d'article sur « zzqxv ».");

    // A call without its argument, and a tool the module does not have.
    for (tool, arguments) in [
        ("web_wikipedia", json!({"query": "  "})),
        ("web_wikipedia", json!({})),
        ("web_search", json!({"query": "x"})),
    ] {
        let mut call = CallToolRequestParams::new(tool);
        call.arguments = arguments.as_object().cloned();
        let result = client.call_tool(call).await.unwrap();
        assert_eq!(result.is_error, Some(true), "{tool}");
    }
}

#[tokio::test]
async fn a_slow_wikipedia_is_an_error_in_time() {
    let (address, _) = serve_fake().await;
    let web = web_at(address, &config("timeout_ms = 300"));
    let started = Instant::now();
    let result = web
        .call("wikipedia", json!({"query": "lent"}).as_object().cloned())
        .await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(result.is_error, Some(true));
    assert_eq!(said(&result), "Wikipédia ne répond pas pour le moment.");
}

#[tokio::test]
async fn an_answer_heavier_than_the_cap_is_given_up() {
    let (address, _) = serve_fake().await;
    let http = Http::new(&WebConfig::default())
        .unwrap()
        .with_max_bytes(1000);
    let url = format!("http://{address}/heavy").parse().unwrap();
    assert_eq!(http.get(url).await.err(), Some(FetchError::TooBig));
    // Under the cap: read whole. And an unreachable server is said so.
    let http = Http::new(&WebConfig::default()).unwrap();
    let url = format!("http://{address}/heavy").parse().unwrap();
    assert_eq!(http.get(url).await.unwrap().body.len(), 3000);
    let url = "http://127.0.0.1:1/".parse().unwrap();
    assert_eq!(http.get(url).await.err(), Some(FetchError::Unreachable));
}

#[test]
fn the_module_is_not_there_when_it_is_disabled() {
    assert!(Web::new(&WebConfig::default()).unwrap().is_some());
    assert!(
        Web::new(&config("enabled = false")).unwrap().is_none(),
        "[web] enabled = false"
    );
    assert!(
        Web::new(&config("[wikipedia]\nenabled = false"))
            .unwrap()
            .is_some(),
        "the weather is left"
    );
    assert!(
        Web::new(&config(
            "[weather]\nenabled = false\n[wikipedia]\nenabled = false"
        ))
        .unwrap()
        .is_none(),
        "no tool left"
    );
}

#[tokio::test]
async fn the_weather_tool_reaches_the_llm_through_emorys_server_and_answers() {
    let (address, seen) = serve_fake().await;
    let web = web_at(
        address,
        &config("user_agent = \"Emory/test (contact)\"\n[weather]\nplace = \"Valence, France\""),
    );
    let server = tools::serve(Tools::new(vec![Arc::new(web)]), ([127, 0, 0, 1], 0).into())
        .await
        .unwrap();
    let client = ()
        .serve(StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(server.url.as_str())
                .auth_header(server.key.clone()),
        ))
        .await
        .unwrap();

    // What the LLM is offered: a short description, two optional arguments.
    let offered = client.list_all_tools().await.unwrap();
    let tool = offered.iter().find(|t| t.name == "web_weather").unwrap();
    let description = tool.description.as_deref().unwrap();
    assert!(description.starts_with("La météo") && description.len() < 120);
    let schema = Value::Object((*tool.input_schema).clone());
    assert!(schema.get("required").is_none());
    assert_eq!(schema["properties"]["place"]["type"], "string");
    assert_eq!(schema["properties"]["days"]["maximum"], 7);

    // No place: the default one, two days; the requests carry the User-Agent of the configuration.
    let mut ask = CallToolRequestParams::new("web_weather");
    ask.arguments = json!({}).as_object().cloned();
    let result = client.call_tool(ask).await.unwrap();
    assert_ne!(result.is_error, Some(true), "{}", said(&result));
    let answer: Value = serde_json::from_str(&said(&result)).unwrap();
    assert_eq!(answer["place"], "Valence");
    assert_eq!(answer["now"]["sky"], "partiellement nuageux");
    assert_eq!(answer["days"][0]["day"], "aujourd'hui");
    assert_eq!(answer["days"][1]["day"], "demain");
    assert_eq!(answer["days"].as_array().unwrap().len(), 2);
    let asked = seen.0.lock().unwrap().clone();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(
        asked[0]
            .0
            .starts_with("/v1/search?name=Valence%2C+France&count=1&language=fr")
    );
    assert!(
        asked[1]
            .0
            .starts_with("/v1/forecast?latitude=44.9256&longitude=4.90956&")
    );
    for (_, agent, accept) in &asked {
        assert_eq!(agent.as_deref(), Some("Emory/test (contact)"));
        assert_eq!(accept.as_deref(), Some("application/json"));
    }

    // Three days, the days passed as text; the same forecast is not asked again.
    let mut ask = CallToolRequestParams::new("web_weather");
    ask.arguments = json!({"days": "3"}).as_object().cloned();
    let answer: Value = serde_json::from_str(&said(&client.call_tool(ask).await.unwrap())).unwrap();
    assert_eq!(answer["days"][2]["day"], "jeudi");
    assert_eq!(seen.0.lock().unwrap().len(), 2);

    // A place nobody knows: a tool error, in one sentence.
    let mut ask = CallToolRequestParams::new("web_weather");
    ask.arguments = json!({"place": "zzqxvkj"}).as_object().cloned();
    let result = client.call_tool(ask).await.unwrap();
    assert_eq!(result.is_error, Some(true));
    assert_eq!(said(&result), "Lieu inconnu : « zzqxvkj ».");
}

#[tokio::test]
async fn a_slow_or_broken_weather_service_is_an_error_in_time() {
    let (address, _) = serve_fake().await;
    let web = web_at(address, &config("timeout_ms = 300"));
    let started = Instant::now();
    let result = web
        .call("weather", json!({"place": "lent"}).as_object().cloned())
        .await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(result.is_error, Some(true));
    assert_eq!(said(&result), "La météo ne répond pas pour le moment.");

    // A forecast service that refuses (400, here: coordinates it does not know).
    let web = web_at(
        address,
        &config("[weather]\nplace = \"x\"\nlatitude = 1.0\nlongitude = 2.0"),
    );
    let result = web.call("weather", json!({}).as_object().cloned()).await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(said(&result), "La météo ne répond pas pour le moment.");

    // No default place and none given.
    let web = web_at(address, &config(""));
    let result = web.call("weather", json!({}).as_object().cloned()).await;
    assert_eq!(result.is_error, Some(true));
    assert!(
        said(&result).contains("Aucun lieu par défaut"),
        "{}",
        said(&result)
    );
}

/// By hand, against the real Open-Meteo (a handful of requests):
/// `cargo test -p mind --test web real_open_meteo -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "reaches the real Open-Meteo"]
async fn real_open_meteo() {
    let web = Web::new(&config("[weather]\nplace = \"Valence, France\""))
        .unwrap()
        .unwrap();
    // The default place (geocoding and forecast), the same again (kept: no request), a place
    // asked with its country, then a nonsense word.
    for arguments in [
        json!({}),
        json!({}),
        json!({"place": "Lyon", "days": 3}),
        json!({"place": "zzqxvkj"}),
    ] {
        let started = Instant::now();
        let result = web.call("weather", arguments.as_object().cloned()).await;
        println!(
            "{arguments}: {} ms, error = {:?}\n  {}",
            started.elapsed().as_millis(),
            result.is_error,
            said(&result)
        );
    }
}

/// By hand, against the real Wikipedia (a handful of requests):
/// `cargo test -p mind --test web real_wikipedia -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "reaches the real Wikipedia"]
async fn real_wikipedia() {
    let web = Web::new(&WebConfig::default()).unwrap().unwrap();
    // The subject is the Eiffel Tower, then a nonsense word (two editions asked).
    for query in ["tour eiffel", "zzqxvkj"] {
        let started = Instant::now();
        let result = web
            .call("wikipedia", json!({ "query": query }).as_object().cloned())
            .await;
        println!(
            "{query:?}: {} ms, error = {:?}\n  {}",
            started.elapsed().as_millis(),
            result.is_error,
            said(&result)
        );
    }
}
