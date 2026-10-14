//! Talker bench: Emory's request, sent straight to the Anthropic API instead of through ACP, to
//! measure what a direct-API talker would cost in time and money.
//!
//! It builds what `mind` builds (the instructions of `prompts/system.fr.md` filled from `[owner]`,
//! the text of a request from `request::build`, the entries Hippocampe finds while the owner
//! speaks, the heads-up of a new conversation) and gives the model Emory's own tools, run for
//! real in-process through `mind`'s modules: Hippocampe (with whatever key the configuration
//! names, a read-only one in development) and the dictionary (the hearing is faked). The answer
//! streams; the bench records the time of the first word, of the first sentence (what `voice`
//! could start saying), of the end, and the tokens and cost of every call.
//!
//! ```sh
//! cargo run --release -p mind --example talker-bench -- run --config dev/mind.toml \
//!     --questions dev/talker-bench.toml --key-file <file with the API key> \
//!     --model claude-haiku-5-5 --thinking none --label haiku --runs 2 --out <folder>
//! cargo run --release -p mind --example talker-bench -- summary --out <folder>
//! ```
//!
//! The key is read from its file at each call and never printed. `--max-requests` and
//! `--max-usd` stop the bench before the budget is exceeded (the count is kept in the output
//! folder, so it holds across runs of the bench).

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use anyhow::{Context, Result, anyhow, bail};
use contract::{TurnId, VocabularyAdd, VocabularyReply};
use futures_util::{StreamExt, future::BoxFuture};
use genai::{
    Client, ModelIden,
    chat::{
        CacheControl, ChatMessage, ChatOptions, ChatRequest, ChatStreamEvent, ContentPart,
        ReasoningEffort, Tool, ToolCall, ToolResponse,
    },
    resolver::{AuthData, AuthResolver},
};
use mind::{
    config::Config,
    conversations::Conversations,
    dictionary::{Dictionary, Vocabulary},
    hippocampe::Hippocampe,
    memory::Memory,
    prompt, request,
    sentences::Splitter,
    tools::{HippocampeTools, Tools},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::time::timeout;

/// Tool rounds after which the bench gives up on a turn.
const MAX_ROUNDS: usize = 5;
/// The longest answer asked of the model, thinking included.
const MAX_TOKENS: u32 = 2048;

/// Anthropic list prices in USD per million tokens, read on 2026-10-13 from
/// <https://platform.claude.com/docs/en/about-claude/pricing> (Haiku 5.5: prompts up to 100 000
/// tokens; cache writes at the 5 minute rate; Sonnet 5.5 cache hits at 0.05x the input price).
struct Prices {
    input: f64,
    cache_write: f64,
    cache_read: f64,
    output: f64,
}

fn prices(model: &str) -> Result<Prices> {
    if model.contains("haiku-5-5") {
        Ok(Prices {
            input: 0.10,
            cache_write: 0.125,
            cache_read: 0.01,
            output: 0.50,
        })
    } else if model.contains("sonnet-5-5") {
        Ok(Prices {
            input: 2.0,
            cache_write: 2.5,
            cache_read: 0.10,
            output: 10.0,
        })
    } else {
        bail!("no price known for {model}")
    }
}

// ---- the questions

#[derive(Deserialize)]
struct Questions {
    conversation: Vec<ConversationDef>,
}

#[derive(Deserialize)]
struct ConversationDef {
    name: String,
    turns: Vec<String>,
}

// ---- what is recorded: one JSON line per API call and one per turn

#[derive(Serialize, Deserialize, Clone, Default)]
struct CallRecord {
    label: String,
    run: usize,
    conversation: String,
    turn: usize,
    round: usize,
    /// From the start of the turn (after the entries were found) to the first text of this call.
    first_text_ms: Option<u64>,
    /// Same, for the first complete sentence.
    first_sentence_ms: Option<u64>,
    /// Same, to the end of the call's stream.
    end_ms: u64,
    /// When its last text arrived and in how many pieces, and when its first thinking did.
    #[serde(default)]
    last_text_ms: Option<u64>,
    #[serde(default)]
    text_chunks: u32,
    #[serde(default)]
    first_reasoning_ms: Option<u64>,
    /// From the sending of this call to its first event of any kind.
    first_event_ms: Option<u64>,
    input_uncached: i64,
    cache_write: i64,
    cache_read: i64,
    output: i64,
    reasoning: i64,
    cost_usd: f64,
    stop: String,
    tools: Vec<String>,
    /// What the model wrote in this call (before its tool calls, or the answer).
    #[serde(default)]
    text: String,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct TurnRecord {
    label: String,
    model: String,
    thinking: String,
    run: usize,
    conversation: String,
    turn: usize,
    question: String,
    answer: String,
    /// How long the search on the question took, and how many entries it found.
    prefetch_ms: u64,
    prefetch_found: usize,
    heads_up_ms: Option<u64>,
    /// Times from the start of the turn, over all its rounds.
    first_text_ms: Option<u64>,
    first_sentence_ms: Option<u64>,
    total_ms: u64,
    tool_rounds: usize,
    tool_calls: Vec<String>,
    calls: usize,
    input_uncached: i64,
    cache_write: i64,
    cache_read: i64,
    output: i64,
    reasoning: i64,
    cost_usd: f64,
    /// `cold` when the first call wrote the cache and read nothing, `warm` when it read it.
    cache: String,
    error: Option<String>,
    /// Filled by the summary from the calls: when the final round (the answer after the tools)
    /// wrote its first text, and whether text came before a tool call (a waiting phrase).
    #[serde(default)]
    useful_ms: Option<u64>,
    #[serde(default)]
    filler: bool,
}

// ---- the key, the budget

fn read_key(path: &Path) -> Result<String> {
    let key = std::fs::read_to_string(path)
        .with_context(|| format!("reading the key file {}", path.display()))?;
    Ok(key.trim().to_owned())
}

/// Hides the key if an error message ever holds it.
fn redact(text: String, key_file: &Path) -> String {
    match read_key(key_file) {
        Ok(key) if !key.is_empty() => text.replace(&key, "[key]"),
        _ => text,
    }
}

/// The requests sent and the money spent so far, kept in a file so the budget holds across runs.
struct Budget {
    file: PathBuf,
    max_requests: u64,
    max_usd: f64,
}

impl Budget {
    fn read(&self) -> (u64, f64) {
        let text = std::fs::read_to_string(&self.file).unwrap_or_default();
        let mut parts = text.split_whitespace();
        (
            parts.next().and_then(|n| n.parse().ok()).unwrap_or(0),
            parts.next().and_then(|n| n.parse().ok()).unwrap_or(0.0),
        )
    }

    fn write(&self, requests: u64, usd: f64) -> Result<()> {
        std::fs::write(&self.file, format!("{requests} {usd:.6}\n"))?;
        Ok(())
    }

    /// Counts a request about to be sent, or refuses it.
    fn take(&self) -> Result<()> {
        let (requests, usd) = self.read();
        if requests >= self.max_requests {
            bail!("budget reached: {requests} requests sent");
        }
        if usd >= self.max_usd {
            bail!("budget reached: {usd:.4} USD spent");
        }
        self.write(requests + 1, usd)
    }

    fn spent(&self, cost: f64) -> Result<()> {
        let (requests, usd) = self.read();
        self.write(requests, usd + cost)
    }
}

// ---- the dictionary's hearing, faked

struct FakeHearing;

impl Vocabulary for FakeHearing {
    fn add(&self, _request: VocabularyAdd) -> BoxFuture<'_, Result<VocabularyReply, String>> {
        Box::pin(async { Ok(VocabularyReply::success(false, true)) })
    }
}

// ---- the command line

struct RunArgs {
    config: PathBuf,
    questions: PathBuf,
    key_file: PathBuf,
    model: String,
    thinking: String,
    label: String,
    runs: usize,
    /// The number of the first run, to add runs to a label already in the folder.
    first_run: usize,
    out: PathBuf,
    only: Option<String>,
    cache: bool,
    max_requests: u64,
    max_usd: f64,
}

fn flags(args: &[String]) -> Result<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let name = flag
            .strip_prefix("--")
            .ok_or_else(|| anyhow!("unexpected argument {flag}"))?;
        let value = it
            .next()
            .ok_or_else(|| anyhow!("{flag} wants a value"))?
            .clone();
        map.insert(name.to_owned(), value);
    }
    Ok(map)
}

impl RunArgs {
    fn parse(args: &[String]) -> Result<Self> {
        let mut f = flags(args)?;
        let mut take = |name: &str| f.remove(name).ok_or_else(|| anyhow!("--{name} is needed"));
        let parsed = Self {
            config: take("config")?.into(),
            questions: take("questions")?.into(),
            key_file: take("key-file")?.into(),
            model: take("model")?,
            thinking: take("thinking").unwrap_or_else(|_| "none".to_owned()),
            label: take("label")?,
            runs: take("runs").unwrap_or_else(|_| "1".to_owned()).parse()?,
            first_run: take("first-run")
                .unwrap_or_else(|_| "1".to_owned())
                .parse()?,
            out: take("out")?.into(),
            only: take("only").ok(),
            cache: take("cache").map(|v| v != "off").unwrap_or(true),
            max_requests: take("max-requests")
                .unwrap_or_else(|_| "80".into())
                .parse()?,
            max_usd: take("max-usd").unwrap_or_else(|_| "3.0".into()).parse()?,
        };
        if let Some(extra) = f.keys().next() {
            bail!("unknown flag --{extra}");
        }
        Ok(parsed)
    }

    fn effort(&self) -> Result<Option<ReasoningEffort>> {
        Ok(Some(match self.thinking.as_str() {
            "none" => ReasoningEffort::Zero,
            // Sonnet 5.5 refuses `thinking: disabled` (which genai sends for `Zero`): its way of
            // not thinking is `between_tools`, set in the body by hand; genai adds nothing.
            "between-tools" => return Ok(None),
            "low" => ReasoningEffort::Low,
            "medium" => ReasoningEffort::Medium,
            other => match other.strip_prefix("budget:") {
                Some(n) => ReasoningEffort::Budget(n.parse()?),
                None => bail!("--thinking is none, between-tools, low, medium or budget:<tokens>"),
            },
        }))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => {
            let args = RunArgs::parse(&args[1..])?;
            let key_file = args.key_file.clone();
            run(args)
                .await
                .map_err(|e| anyhow!(redact(format!("{e:#}"), &key_file)))
        }
        Some("summary") => {
            let f = flags(&args[1..])?;
            summary(Path::new(
                f.get("out").ok_or_else(|| anyhow!("--out is needed"))?,
            ))
        }
        _ => bail!("usage: talker-bench run|summary --..."),
    }
}

// ---- the run

fn genai_tool(tool: &rmcp::model::Tool) -> Tool {
    Tool::new(tool.name.to_string())
        .with_description(tool.description.as_deref().unwrap_or_default().to_owned())
        .with_schema(Value::Object((*tool.input_schema).clone()))
}

/// The text of a tool result, as the LLM reads it.
fn result_text(result: &rmcp::model::CallToolResult) -> String {
    let mut text: String = result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty()
        && let Some(structured) = &result.structured_content
    {
        text = structured.to_string();
    }
    if result.is_error == Some(true) {
        text = format!("Erreur : {text}");
    }
    text
}

async fn run(args: RunArgs) -> Result<()> {
    let effort = args.effort()?;
    let price = prices(&args.model)?;
    std::fs::create_dir_all(&args.out)?;
    let budget = Budget {
        file: args.out.join("budget.txt"),
        max_requests: args.max_requests,
        max_usd: args.max_usd,
    };

    let config = Config::read(&args.config)?;
    let system = prompt::render(
        &std::fs::read_to_string(&config.llm.system_prompt)
            .with_context(|| format!("reading {}", config.llm.system_prompt.0.display()))?,
        &config.owner,
    );
    let questions: Questions = toml::from_str(&std::fs::read_to_string(&args.questions)?)?;

    let hippocampe = Hippocampe::new(
        config.hippocampe.url.clone(),
        config.hippocampe.token_file.0.clone(),
    );
    hippocampe.connect().await?;
    let conversations = Conversations::default();
    let tools = Tools::new(vec![
        Arc::new(HippocampeTools::new(
            hippocampe.clone(),
            config.hippocampe.tools.clone(),
            config.hippocampe.tool_timeout(),
        )),
        Arc::new(Dictionary::new(FakeHearing, conversations.clone())),
    ]);
    let offered: Vec<Tool> = tools.list().await.iter().map(genai_tool).collect();
    eprintln!(
        "tools offered: {}",
        offered
            .iter()
            .map(|t| t.name.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );

    // The key is read from its file each time the client asks for it, and goes nowhere else.
    let key_file = args.key_file.clone();
    let client = Client::builder()
        .with_auth_resolver(AuthResolver::from_resolver_fn(
            move |_model: ModelIden| -> Result<Option<AuthData>, genai::resolver::Error> {
                let key = read_key(&key_file)
                    .map_err(|e| genai::resolver::Error::Custom(e.to_string()))?;
                Ok(Some(AuthData::from_single(key)))
            },
        ))
        .build()?;

    let between_tools = args.thinking == "between-tools";
    let mut options = ChatOptions::default()
        .with_capture_usage(true)
        .with_capture_content(true)
        .with_capture_tool_calls(true)
        .with_max_tokens(MAX_TOKENS);
    if let Some(effort) = &effort {
        options = options.with_reasoning_effort(effort.clone());
    }
    if between_tools {
        options = options.with_extra_body(json!({"thinking": {"type": "between_tools"}}));
    }
    if !matches!(effort, Some(ReasoningEffort::Zero)) {
        options = options.with_capture_reasoning_content(true);
    }

    let log = args.out.join(format!("{}.jsonl", args.label));
    let answers = args.out.join(format!("{}-answers.md", args.label));
    for run in args.first_run..args.first_run + args.runs {
        for def in &questions.conversation {
            if args.only.as_ref().is_some_and(|o| o != &def.name) {
                continue;
            }
            let key = conversations.open();
            let mut history: Vec<ChatMessage> = Vec::new();
            let heads_up_started = Instant::now();
            let heads_up = hippocampe.heads_up().await.ok().flatten();
            let heads_up_ms = heads_up_started.elapsed().as_millis() as u64;
            for (index, question) in def.turns.iter().enumerate() {
                conversations.spoke(&key, TurnId::new());
                // The entries found while the owner speaks, as `mind` searches them.
                let started = Instant::now();
                let found = match timeout(
                    config.hippocampe.search_timeout(),
                    hippocampe.search(question, config.hippocampe.search_limit),
                )
                .await
                {
                    Ok(Ok(found)) => found,
                    _ => Vec::new(),
                };
                let prefetch_ms = started.elapsed().as_millis() as u64;
                let found = request::merge([found]);
                let text = request::build(
                    contract::now(),
                    heads_up.as_deref().filter(|_| index == 0),
                    &[],
                    &found,
                    question,
                );
                history.push(ChatMessage::user(text));

                let mut turn = TurnRecord {
                    label: args.label.clone(),
                    model: args.model.clone(),
                    thinking: args.thinking.clone(),
                    run,
                    conversation: def.name.clone(),
                    turn: index,
                    question: question.clone(),
                    prefetch_ms,
                    prefetch_found: found.len(),
                    heads_up_ms: (index == 0).then_some(heads_up_ms),
                    ..Default::default()
                };
                let outcome = ask(
                    &client,
                    &args,
                    &price,
                    &budget,
                    &options,
                    &system,
                    &offered,
                    &tools,
                    &key,
                    &mut history,
                    &mut turn,
                    &log,
                )
                .await;
                if let Err(e) = outcome {
                    let message = redact(format!("{e:#}"), &args.key_file);
                    eprintln!(
                        "[{} run {run} {} {index}] error: {message}",
                        args.label, def.name
                    );
                    let stop = message.contains("budget reached");
                    turn.error = Some(message);
                    append(&log, &json!({"kind": "turn", "turn": turn}))?;
                    if stop {
                        return Err(anyhow!("stopped: the budget is reached"));
                    }
                    // A failed turn leaves the history unusable for the rest of the conversation.
                    break;
                }
                append(&log, &json!({"kind": "turn", "turn": turn}))?;
                let mut note = String::new();
                let _ = write!(
                    note,
                    "## {} / run {} / {} / {}\n\n> {}\n\n{}\n\n_{} | first word {} ms, first sentence {} ms, total {} ms, tools {:?}, ${:.5}_\n\n",
                    args.label,
                    run,
                    def.name,
                    index + 1,
                    question,
                    turn.answer,
                    turn.cache,
                    turn.first_text_ms.map_or("-".into(), |v| v.to_string()),
                    turn.first_sentence_ms.map_or("-".into(), |v| v.to_string()),
                    turn.total_ms,
                    turn.tool_calls,
                    turn.cost_usd,
                );
                append_text(&answers, &note)?;
                eprintln!(
                    "[{} run {run} {} {}] first word {:?} ms, total {} ms, tools {}, ${:.5}",
                    args.label,
                    def.name,
                    index + 1,
                    turn.first_text_ms,
                    turn.total_ms,
                    turn.tool_calls.len(),
                    turn.cost_usd
                );
            }
            conversations.close(&key);
        }
    }
    let (requests, usd) = budget.read();
    eprintln!("budget so far: {requests} requests, {usd:.4} USD");
    Ok(())
}

/// One turn: the request, the tool rounds, the final answer.
#[allow(clippy::too_many_arguments)]
async fn ask(
    client: &Client,
    args: &RunArgs,
    price: &Prices,
    budget: &Budget,
    options: &ChatOptions,
    system: &str,
    offered: &[Tool],
    tools: &Tools,
    key: &mind::conversations::ConversationKey,
    history: &mut Vec<ChatMessage>,
    turn: &mut TurnRecord,
    log: &Path,
) -> Result<()> {
    let started = Instant::now();
    // As in `mind`, the text of all the rounds is one stream: what the model writes before a
    // tool call is said at once.
    let mut splitter = Splitter::new();
    let mut spoken = String::new();
    for round in 0..MAX_ROUNDS {
        let mut system_message = ChatMessage::system(system.to_owned());
        if args.cache {
            system_message = system_message.with_options(CacheControl::Ephemeral);
        }
        let mut messages = vec![system_message];
        messages.extend(history.iter().cloned());
        let request = ChatRequest::new(messages).with_tools(offered.to_vec());

        budget.take()?;
        let sent = Instant::now();
        let mut stream = client
            .exec_chat_stream(&args.model, request, Some(options))
            .await
            .map_err(|e| anyhow!("{e}"))?;
        let mut call = CallRecord {
            label: args.label.clone(),
            run: turn.run,
            conversation: turn.conversation.clone(),
            turn: turn.turn,
            round,
            ..Default::default()
        };
        let mut text = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut content = None;
        while let Some(event) = stream.stream.next().await {
            let event = event.map_err(|e| anyhow!("{e}"))?;
            call.first_event_ms
                .get_or_insert(sent.elapsed().as_millis() as u64);
            match event {
                ChatStreamEvent::Chunk(chunk) if !chunk.content.is_empty() => {
                    let now = started.elapsed().as_millis() as u64;
                    call.first_text_ms.get_or_insert(now);
                    call.last_text_ms = Some(now);
                    call.text_chunks += 1;
                    turn.first_text_ms.get_or_insert(now);
                    if !splitter.push(&chunk.content).is_empty() {
                        call.first_sentence_ms.get_or_insert(now);
                        turn.first_sentence_ms.get_or_insert(now);
                    }
                    text.push_str(&chunk.content);
                }
                ChatStreamEvent::ReasoningChunk(_) => {
                    call.first_reasoning_ms
                        .get_or_insert(started.elapsed().as_millis() as u64);
                }
                ChatStreamEvent::End(end) => {
                    if let Some(usage) = end.captured_usage {
                        let total = usage.prompt_tokens.unwrap_or(0) as i64;
                        let details = usage.prompt_tokens_details.unwrap_or_default();
                        call.cache_write = details.cache_creation_tokens.unwrap_or(0) as i64;
                        call.cache_read = details.cached_tokens.unwrap_or(0) as i64;
                        call.input_uncached = total - call.cache_write - call.cache_read;
                        call.output = usage.completion_tokens.unwrap_or(0) as i64;
                        call.reasoning = usage
                            .completion_tokens_details
                            .and_then(|d| d.reasoning_tokens)
                            .unwrap_or(0) as i64;
                    }
                    call.stop = end
                        .captured_stop_reason
                        .map_or_else(String::new, |s| format!("{s:?}"));
                    if let Some(c) = end.captured_content {
                        tool_calls = c
                            .parts()
                            .iter()
                            .filter_map(|p| match p {
                                ContentPart::ToolCall(tc) => Some(tc.clone()),
                                _ => None,
                            })
                            .collect();
                        content = Some(c);
                    }
                }
                _ => {}
            }
        }
        call.end_ms = started.elapsed().as_millis() as u64;
        // What is pending leaves at the end of the stream, before a tool call or at the end of the
        // answer, as in `mind`.
        if splitter.flush().is_some() {
            call.first_sentence_ms.get_or_insert(call.end_ms);
            turn.first_sentence_ms.get_or_insert(call.end_ms);
        }
        call.cost_usd = (call.input_uncached as f64 * price.input
            + call.cache_write as f64 * price.cache_write
            + call.cache_read as f64 * price.cache_read
            + call.output as f64 * price.output)
            / 1e6;
        call.tools = tool_calls.iter().map(|t| t.fn_name.clone()).collect();
        call.text = text.clone();
        if !spoken.is_empty() && !text.is_empty() {
            spoken.push('\n');
        }
        spoken.push_str(&text);
        budget.spent(call.cost_usd)?;
        turn.calls += 1;
        turn.input_uncached += call.input_uncached;
        turn.cache_write += call.cache_write;
        turn.cache_read += call.cache_read;
        turn.output += call.output;
        turn.reasoning += call.reasoning;
        turn.cost_usd += call.cost_usd;
        if round == 0 {
            turn.cache = if call.cache_read > 0 {
                "warm"
            } else if call.cache_write > 0 {
                "cold"
            } else {
                "none"
            }
            .to_owned();
        }
        append(log, &json!({"kind": "call", "call": call}))?;

        if tool_calls.is_empty() {
            turn.answer = spoken;
            turn.total_ms = started.elapsed().as_millis() as u64;
            history.push(ChatMessage::assistant(text));
            return Ok(());
        }
        // The model called tools: run them, give the results back.
        turn.tool_rounds += 1;
        history.push(ChatMessage::assistant(content.unwrap_or_default()));
        for call in tool_calls {
            let arguments = call.fn_arguments.as_object().cloned();
            let result = tools
                .call_in(&call.fn_name, arguments, Some(key.clone()))
                .await;
            turn.tool_calls.push(call.fn_name.clone());
            history.push(ChatMessage::from(ToolResponse::new(
                call.call_id,
                result_text(&result),
            )));
        }
    }
    bail!("more than {MAX_ROUNDS} tool rounds")
}

fn append(path: &Path, value: &Value) -> Result<()> {
    append_text(path, &format!("{value}\n"))
}

fn append_text(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(text.as_bytes())?;
    Ok(())
}

// ---- the summary

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|a, b| a.total_cmp(b));
    match values.len() {
        0 => f64::NAN,
        n if n % 2 == 1 => values[n / 2],
        n => (values[n / 2 - 1] + values[n / 2]) / 2.0,
    }
}

fn stats(turns: &[&TurnRecord], pick: impl Fn(&TurnRecord) -> Option<u64>) -> String {
    let mut values: Vec<f64> = turns
        .iter()
        .filter_map(|t| pick(t))
        .map(|v| v as f64)
        .collect();
    if values.is_empty() {
        return "-".to_owned();
    }
    let worst = values.iter().cloned().fold(0.0, f64::max);
    format!("{:.0} / {:.0}", median(&mut values), worst)
}

fn summary(out: &Path) -> Result<()> {
    let mut by_label: BTreeMap<String, Vec<TurnRecord>> = BTreeMap::new();
    let mut calls: BTreeMap<(String, usize, String, usize), Vec<CallRecord>> = BTreeMap::new();
    for entry in std::fs::read_dir(out)? {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "jsonl") {
            continue;
        }
        for line in std::fs::read_to_string(&path)?.lines() {
            let value: Value = serde_json::from_str(line)?;
            if value["kind"] == "turn" {
                let turn: TurnRecord = serde_json::from_value(value["turn"].clone())?;
                if turn.error.is_none() {
                    by_label.entry(turn.label.clone()).or_default().push(turn);
                }
            } else {
                let call: CallRecord = serde_json::from_value(value["call"].clone())?;
                calls
                    .entry((
                        call.label.clone(),
                        call.run,
                        call.conversation.clone(),
                        call.turn,
                    ))
                    .or_default()
                    .push(call);
            }
        }
    }
    for turn in by_label.values_mut().flatten() {
        let rounds = calls
            .get(&(
                turn.label.clone(),
                turn.run,
                turn.conversation.clone(),
                turn.turn,
            ))
            .cloned()
            .unwrap_or_default();
        turn.useful_ms = rounds.last().and_then(|c| c.first_text_ms);
        turn.filler = rounds
            .iter()
            .any(|c| !c.tools.is_empty() && !c.text.trim().is_empty());
    }
    let mut text = String::new();
    let _ = writeln!(
        text,
        "| variant | group | turns | waiting phrase | first word ms (median / worst) | first sentence ms | first useful word ms | total ms | input uncached | cache write | cache read | output | USD per answer | USD per month (50 a day) |"
    );
    let _ = writeln!(
        text,
        "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"
    );
    for (label, turns) in &by_label {
        let groups: Vec<(&str, Vec<&TurnRecord>)> = vec![
            ("all", turns.iter().collect()),
            (
                "without tools",
                turns.iter().filter(|t| t.tool_rounds == 0).collect(),
            ),
            (
                "with tools",
                turns.iter().filter(|t| t.tool_rounds > 0).collect(),
            ),
            (
                "cold cache",
                turns.iter().filter(|t| t.cache == "cold").collect(),
            ),
            (
                "warm cache",
                turns.iter().filter(|t| t.cache == "warm").collect(),
            ),
        ];
        for (group, members) in groups {
            if members.is_empty() {
                continue;
            }
            let n = members.len() as f64;
            let mean =
                |f: &dyn Fn(&TurnRecord) -> f64| members.iter().map(|t| f(t)).sum::<f64>() / n;
            let cost = mean(&|t| t.cost_usd);
            let _ = writeln!(
                text,
                "| {label} | {group} | {} | {} | {} | {} | {} | {} | {:.0} | {:.0} | {:.0} | {:.0} | {:.5} | {:.2} |",
                members.len(),
                members.iter().filter(|t| t.filler).count(),
                stats(&members, |t| t.first_text_ms),
                stats(&members, |t| t.first_sentence_ms),
                stats(&members, |t| t.useful_ms),
                stats(&members, |t| Some(t.total_ms)),
                mean(&|t| t.input_uncached as f64),
                mean(&|t| t.cache_write as f64),
                mean(&|t| t.cache_read as f64),
                mean(&|t| t.output as f64),
                cost,
                cost * 50.0 * 30.0,
            );
        }
    }
    let (requests, usd) = Budget {
        file: out.join("budget.txt"),
        max_requests: 0,
        max_usd: 0.0,
    }
    .read();
    let _ = writeln!(text, "\nRequests sent: {requests}. Spent: {usd:.4} USD.");
    print!("{text}");
    std::fs::write(out.join("summary.md"), &text)?;
    Ok(())
}
