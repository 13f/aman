#![forbid(unsafe_code)]
#![doc = "Skill registry, loader, and trigger execution for the aman agent framework."]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


use kernel::context::SkillContext;
use kernel::event::Event;
use kernel::skill::{Skill, TriggerCondition};
use kernel::{AmanResult, Error};
use notify::Watcher;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, Schema, STORED, STRING, TEXT, Value as TantivyValue};
use tantivy::{doc, Index, IndexReader, IndexWriter, TantivyDocument, Term};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SkillConcurrencyModel {
    #[default]
    Serial,
    Parallel,
    Limited(usize),
}

#[derive(Clone)]
struct SkillRegistration {
    skill: Arc<dyn Skill>,
    enabled: bool,
    concurrency: SkillConcurrencyModel,
}

#[derive(Clone)]
struct EnabledSkillEntry {
    skill: Arc<dyn Skill>,
    concurrency: SkillConcurrencyModel,
}

#[derive(Default)]
pub struct SkillRegistry {
    skills: RwLock<HashMap<String, SkillRegistration>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSnapshot {
    pub name: String,
    pub version: String,
    pub description: String,
    pub enabled: bool,
    pub concurrency: SkillConcurrencyModel,
    #[serde(default)]
    pub triggers: Vec<TriggerCondition>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillUpsertOutcome {
    Inserted,
    ReplacedSameVersion {
        version: Version,
    },
    ReplacedNewVersion {
        old_version: Version,
        new_version: Version,
    },
}

impl SkillRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, skill: Arc<dyn Skill>) -> AmanResult<()> {
        self.register_with_model(skill, SkillConcurrencyModel::Serial)
    }

    pub fn register_with_model(
        &self,
        skill: Arc<dyn Skill>,
        concurrency: SkillConcurrencyModel,
    ) -> AmanResult<()> {
        let name = skill.name().to_owned();
        let mut skills = self.skills.write().expect("skill registry write lock");
        if skills.contains_key(&name) {
            return Err(Error::AlreadyExists {
                name: format!("skill:{name}"),
            });
        }
        skills.insert(
            name,
            SkillRegistration {
                skill,
                enabled: true,
                concurrency,
            },
        );
        Ok(())
    }

    pub fn register_loaded(&self, loaded: LoadedSkill) -> AmanResult<()> {
        self.register_with_model(loaded.skill, loaded.concurrency)
    }

    pub fn upsert_loaded(&self, loaded: LoadedSkill) -> SkillUpsertOutcome {
        let name = loaded.skill.name().to_owned();
        let mut skills = self.skills.write().expect("skill registry write lock");
        if let Some(existing) = skills.get_mut(&name) {
            let old_version = existing.skill.version().clone();
            let new_version = loaded.skill.version().clone();
            existing.skill = loaded.skill;
            existing.concurrency = loaded.concurrency;
            if old_version == new_version {
                return SkillUpsertOutcome::ReplacedSameVersion {
                    version: new_version,
                };
            }
            return SkillUpsertOutcome::ReplacedNewVersion {
                old_version,
                new_version,
            };
        }

        skills.insert(
            name,
            SkillRegistration {
                skill: loaded.skill,
                enabled: true,
                concurrency: loaded.concurrency,
            },
        );
        SkillUpsertOutcome::Inserted
    }

    pub fn set_concurrency(&self, name: &str, concurrency: SkillConcurrencyModel) -> AmanResult<()> {
        let mut skills = self.skills.write().expect("skill registry write lock");
        let Some(entry) = skills.get_mut(name) else {
            return Err(Error::NotFound {
                name: format!("skill:{name}"),
            });
        };
        entry.concurrency = concurrency;
        Ok(())
    }

    pub fn enable(&self, name: &str) -> AmanResult<()> {
        let mut skills = self.skills.write().expect("skill registry write lock");
        let Some(entry) = skills.get_mut(name) else {
            return Err(Error::NotFound {
                name: format!("skill:{name}"),
            });
        };
        entry.enabled = true;
        Ok(())
    }

    pub fn disable(&self, name: &str) -> AmanResult<()> {
        let mut skills = self.skills.write().expect("skill registry write lock");
        let Some(entry) = skills.get_mut(name) else {
            return Err(Error::NotFound {
                name: format!("skill:{name}"),
            });
        };
        entry.enabled = false;
        Ok(())
    }

    pub fn unregister(&self, name: &str) -> AmanResult<()> {
        let mut skills = self.skills.write().expect("skill registry write lock");
        if skills.remove(name).is_none() {
            return Err(Error::NotFound {
                name: format!("skill:{name}"),
            });
        }
        Ok(())
    }

    #[must_use]
    pub fn snapshot(&self, name: &str) -> Option<SkillSnapshot> {
        self.skills
            .read()
            .expect("skill registry read lock")
            .get(name)
            .map(|entry| SkillSnapshot {
                name: entry.skill.name().to_owned(),
                version: entry.skill.version().to_string(),
                description: entry.skill.description().to_owned(),
                enabled: entry.enabled,
                concurrency: entry.concurrency,
                triggers: entry.skill.triggers().to_vec(),
            })
    }

    #[must_use]
    pub fn list(&self) -> Vec<SkillSnapshot> {
        let mut items = self
            .skills
            .read()
            .expect("skill registry read lock")
            .values()
            .map(|entry| SkillSnapshot {
                name: entry.skill.name().to_owned(),
                version: entry.skill.version().to_string(),
                description: entry.skill.description().to_owned(),
                enabled: entry.enabled,
                concurrency: entry.concurrency,
                triggers: entry.skill.triggers().to_vec(),
            })
            .collect::<Vec<_>>();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        items
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn Skill>> {
        self.skills
            .read()
            .expect("skill registry read lock")
            .get(name)
            .map(|entry| Arc::clone(&entry.skill))
    }

    #[must_use]
    pub fn model_of(&self, name: &str) -> Option<SkillConcurrencyModel> {
        self.skills
            .read()
            .expect("skill registry read lock")
            .get(name)
            .map(|entry| entry.concurrency)
    }

    #[must_use]
    pub fn enabled_skills(&self) -> Vec<Arc<dyn Skill>> {
        self.skills
            .read()
            .expect("skill registry read lock")
            .values()
            .filter(|entry| entry.enabled)
            .map(|entry| Arc::clone(&entry.skill))
            .collect()
    }

    /// Drain all registered skills (Phase 4.5).
    /// Each skill's `drain()` is called to close per-session queues and
    /// cancel in-flight work. Returns the total number of drained items.
    pub fn drain_all(&self) -> usize {
        let skills = self.skills.read().expect("skill registry read lock");
        let mut total = 0;
        for registration in skills.values() {
            total += registration.skill.drain();
        }
        total
    }

    fn enabled_entries(&self) -> Vec<EnabledSkillEntry> {
        self.skills
            .read()
            .expect("skill registry read lock")
            .values()
            .filter(|entry| entry.enabled)
            .map(|entry| EnabledSkillEntry {
                skill: Arc::clone(&entry.skill),
                concurrency: entry.concurrency,
            })
            .collect()
    }
}

#[must_use]
pub fn trigger_matches(condition: &TriggerCondition, event: &Event) -> bool {
    let event_type_match = condition.event_types.is_empty()
        || condition
            .event_types
            .iter()
            .any(|expected| expected == &event.event_type);
    let source_match =
        condition.sources.is_empty() || condition.sources.iter().any(|source| source == &event.source);
    let priority_match = condition.priorities.is_empty()
        || condition
            .priorities
            .iter()
            .any(|priority| priority == &event.priority);

    if condition.match_all {
        return event_type_match && source_match && priority_match;
    }

    let has_any_rule = !condition.event_types.is_empty()
        || !condition.sources.is_empty()
        || !condition.priorities.is_empty();
    if !has_any_rule {
        return true;
    }

    condition
        .event_types
        .iter()
        .any(|expected| expected == &event.event_type)
        || condition.sources.iter().any(|source| source == &event.source)
        || condition
            .priorities
            .iter()
            .any(|priority| priority == &event.priority)
}

#[must_use]
pub fn skill_matches_event(skill: &dyn Skill, event: &Event) -> bool {
    let triggers = skill.triggers();
    !triggers.is_empty() && triggers.iter().any(|condition| trigger_matches(condition, event))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillFailure {
    pub skill_name: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SkillDispatchResult {
    pub executed: Vec<String>,
    pub failed: Vec<SkillFailure>,
}

#[derive(Default)]
struct SkillConcurrencyController {
    states: Mutex<HashMap<String, Arc<SkillConcurrencyState>>>,
}

impl SkillConcurrencyController {
    fn enter(&self, skill_name: &str, model: SkillConcurrencyModel) -> SkillConcurrencyGuard {
        let state = {
            let mut states = self.states.lock().expect("skill concurrency states lock");
            Arc::clone(
                states
                    .entry(skill_name.to_owned())
                    .or_insert_with(|| Arc::new(SkillConcurrencyState::default())),
            )
        };

        let mut running = state.running.lock().expect("skill running count lock");
        match model {
            SkillConcurrencyModel::Serial => {
                while *running > 0 {
                    running = state.wakeup.wait(running).expect("skill wakeup wait");
                }
                *running = 1;
            }
            SkillConcurrencyModel::Limited(limit) => {
                let limit = limit.max(1);
                while *running >= limit {
                    running = state.wakeup.wait(running).expect("skill wakeup wait");
                }
                *running += 1;
            }
            SkillConcurrencyModel::Parallel => {
                *running += 1;
            }
        }
        drop(running);
        SkillConcurrencyGuard { state }
    }
}

#[derive(Default)]
struct SkillConcurrencyState {
    running: Mutex<usize>,
    wakeup: Condvar,
}

struct SkillConcurrencyGuard {
    state: Arc<SkillConcurrencyState>,
}

impl Drop for SkillConcurrencyGuard {
    fn drop(&mut self) {
        let mut running = self.state.running.lock().expect("skill running count lock");
        *running = running.saturating_sub(1);
        if *running == 0 {
            self.state.wakeup.notify_all();
        } else {
            self.state.wakeup.notify_one();
        }
    }
}

pub struct SkillExecutor {
    registry: Arc<SkillRegistry>,
    concurrency: SkillConcurrencyController,
    inflight_skills: Arc<std::sync::atomic::AtomicUsize>,
}

impl SkillExecutor {
    #[must_use]
    pub fn new(registry: Arc<SkillRegistry>) -> Self {
        Self {
            registry,
            concurrency: SkillConcurrencyController::default(),
            inflight_skills: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Number of skills currently being executed.
    #[must_use]
    pub fn inflight_count(&self) -> usize {
        self.inflight_skills.load(std::sync::atomic::Ordering::Acquire)
    }

    pub async fn execute_matching(&self, event: Event, ctx: SkillContext) -> SkillDispatchResult {
        self.inflight_skills.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let result = self.execute_matching_inner(event, ctx).await;
        self.inflight_skills.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        result
    }

    async fn execute_matching_inner(&self, event: Event, ctx: SkillContext) -> SkillDispatchResult {
        let entries = self.registry.enabled_entries();
        let mut result = SkillDispatchResult::default();
        for entry in entries {
            if !skill_matches_event(entry.skill.as_ref(), &event) {
                continue;
            }
            let _guard = self
                .concurrency
                .enter(entry.skill.name(), entry.concurrency);
            match entry.skill.execute(event.clone(), ctx.clone()).await {
                Ok(()) => result.executed.push(entry.skill.name().to_owned()),
                Err(error) => result.failed.push(SkillFailure {
                    skill_name: entry.skill.name().to_owned(),
                    message: error.to_string(),
                }),
            }
        }
        result
    }
}

#[derive(Clone)]
pub struct LoadedSkill {
    pub skill: Arc<dyn Skill>,
    pub concurrency: SkillConcurrencyModel,
}

#[derive(Debug, Deserialize)]
struct DeclarativeSkillSpec {
    name: String,
    version: String,
    description: Option<String>,
    #[serde(default)]
    triggers: Vec<TriggerCondition>,
    concurrency: Option<serde_yaml::Value>,
}

#[derive(Debug)]
struct DeclarativeSkill {
    name: String,
    version: Version,
    description: String,
    triggers: Vec<TriggerCondition>,
}

#[async_trait::async_trait]
impl Skill for DeclarativeSkill {
    fn name(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &Version {
        &self.version
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn triggers(&self) -> &[TriggerCondition] {
        &self.triggers
    }

    async fn execute(&self, _event: Event, _ctx: SkillContext) -> AmanResult<()> {
        Ok(())
    }
}

pub struct SkillLoader;

impl SkillLoader {
    pub fn load_from_yaml_str(content: &str) -> AmanResult<LoadedSkill> {
        let spec: DeclarativeSkillSpec = serde_yaml::from_str(content).map_err(|error| {
            Error::ConfigInvalid {
                message: format!("invalid skill yaml: {error}"),
            }
        })?;
        Self::build_loaded_skill(spec)
    }

    pub fn load_from_yaml_file(path: &Path) -> AmanResult<LoadedSkill> {
        let content = fs::read_to_string(path)?;
        Self::load_from_yaml_str(&content)
    }

    pub fn load_from_skill_markdown_str(content: &str) -> AmanResult<LoadedSkill> {
        let yaml = extract_skill_markdown_yaml(content)?;
        Self::load_from_yaml_str(&yaml)
    }

    pub fn load_from_skill_markdown_file(path: &Path) -> AmanResult<LoadedSkill> {
        let content = fs::read_to_string(path)?;
        Self::load_from_skill_markdown_str(&content)
    }

    pub fn discover_skill_files(root: &Path) -> AmanResult<Vec<PathBuf>> {
        let mut found = Vec::new();
        discover_files_recursive(root, &mut found)?;
        found.sort();
        Ok(found)
    }

    pub fn load_from_path(path: &Path) -> AmanResult<LoadedSkill> {
        match path.extension().and_then(|ext| ext.to_str()) {
            Some("yaml") | Some("yml") => Self::load_from_yaml_file(path),
            Some("md") if path.file_name().and_then(|name| name.to_str()) == Some("SKILL.md") => {
                Self::load_from_skill_markdown_file(path)
            }
            _ => Err(Error::ConfigInvalid {
                message: format!("unsupported skill file: {}", path.display()),
            }),
        }
    }

    fn build_loaded_skill(spec: DeclarativeSkillSpec) -> AmanResult<LoadedSkill> {
        let version = Version::parse(&spec.version).map_err(|error| Error::ConfigInvalid {
            message: format!("invalid skill version `{}`: {error}", spec.version),
        })?;
        let skill = DeclarativeSkill {
            name: spec.name,
            version,
            description: spec
                .description
                .unwrap_or_else(|| "declarative skill".to_owned()),
            triggers: spec.triggers,
        };
        Ok(LoadedSkill {
            skill: Arc::new(skill),
            concurrency: parse_skill_concurrency(spec.concurrency)?,
        })
    }
}

// ---------------------------------------------------------------------------
// LLM instruction skills (Agent Skills standard — SKILL.md with frontmatter)
// ---------------------------------------------------------------------------

/// An LLM-instruction skill loaded from a SKILL.md file (Agent Skills standard).
///
/// These are NOT event-driven. The LLM decides when to use them based on
/// the `name` and `description` injected into its context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub category: String,
    pub triggers: Vec<String>,
    #[serde(skip)]
    pub path: PathBuf,
}

pub mod execution;
pub mod formatting;

pub mod skm_adapter;
#[doc(inline)]
pub use skm_adapter::SkmRegistry;

pub mod validation;
#[doc(inline)]
pub use validation::*;

pub mod export;
#[doc(inline)]
pub use export::*;

/// Discover all LLM-instruction skills under a directory by walking recursively
/// for SKILL.md files that contain YAML frontmatter (Agent Skills convention).
///
/// Delegates to [`SkmRegistry`] backed by skm-core's spec-compliant parser.
pub fn discover_llm_skills(root: &Path) -> Vec<SkillInfo> {
    skm_adapter::SkmRegistry::new(root).discover()
}

#[derive(Debug, Clone)]
pub struct IndexedSkill {
    pub name: String,
    pub version: String,
    pub description: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMatch {
    pub name: String,
    pub version: String,
    pub score: u32,
    pub snippet: String,
    pub matched_field: String,
}

pub struct SkillSearch {
    state: Mutex<SkillSearchState>,
}

struct SkillSearchState {
    fields: SkillSearchFields,
    reader: IndexReader,
    writer: IndexWriter,
    skills: HashMap<String, IndexedSkill>,
}

#[derive(Clone, Copy)]
struct SkillSearchFields {
    name: Field,
    name_raw: Field,
    version: Field,
    description: Field,
    tags: Field,
}

impl SkillSearch {
    #[must_use]
    pub fn new() -> Self {
        let mut schema_builder = Schema::builder();
        let name = schema_builder.add_text_field("name", TEXT | STORED);
        let name_raw = schema_builder.add_text_field("name_raw", STRING | STORED);
        let version = schema_builder.add_text_field("version", TEXT | STORED);
        let description = schema_builder.add_text_field("description", TEXT | STORED);
        let tags = schema_builder.add_text_field("tags", TEXT | STORED);
        let schema = schema_builder.build();

        let index = Index::create_in_ram(schema);
        let writer = index
            .writer(15_000_000)
            .expect("tantivy writer should initialize");
        let reader = index
            .reader()
            .expect("tantivy reader should initialize");

        Self {
            state: Mutex::new(SkillSearchState {
                fields: SkillSearchFields {
                    name,
                    name_raw,
                    version,
                    description,
                    tags,
                },
                reader,
                writer,
                skills: HashMap::new(),
            }),
        }
    }
}

impl Default for SkillSearch {
    fn default() -> Self {
        Self::new()
    }
}

impl SkillSearch {
    /// Index a skill for search.
    pub fn index_skill(&self, skill: IndexedSkill) {
        let mut state = self.state.lock().expect("skill search state lock");
        state.writer.delete_term(Term::from_field_text(
            state.fields.name_raw,
            &skill.name,
        ));

        let mut document = doc!(
            state.fields.name => skill.name.clone(),
            state.fields.name_raw => skill.name.clone(),
            state.fields.version => skill.version.clone(),
            state.fields.description => skill.description.clone(),
        );
        for tag in &skill.tags {
            document.add_text(state.fields.tags, tag);
        }

        let _ = state
            .writer
            .add_document(document)
            .expect("tantivy add_document should succeed");
        state
            .writer
            .commit()
            .expect("tantivy commit should succeed");
        state
            .reader
            .reload()
            .expect("tantivy reader reload should succeed");
        state.skills.insert(skill.name.clone(), skill);
    }

    pub fn index_runtime_skill(&self, skill: &dyn Skill) {
        self.index_skill(IndexedSkill {
            name: skill.name().to_owned(),
            version: skill.version().to_string(),
            description: skill.description().to_owned(),
            tags: skill
                .triggers()
                .iter()
                .flat_map(|trigger| trigger.event_types.iter().map(ToString::to_string))
                .collect(),
        });
    }

    pub fn remove_skill(&self, name: &str) {
        let mut state = self.state.lock().expect("skill search state lock");
        state
            .writer
            .delete_term(Term::from_field_text(state.fields.name_raw, name));
        state
            .writer
            .commit()
            .expect("tantivy commit should succeed");
        state
            .reader
            .reload()
            .expect("tantivy reader reload should succeed");
        state.skills.remove(name);
    }

    #[must_use]
    pub fn search(&self, query: &str, limit: usize) -> Vec<SkillMatch> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let limit = limit.max(1);

        let state = self.state.lock().expect("skill search state lock");
        let parser = QueryParser::for_index(
            state.reader.searcher().index(),
            vec![state.fields.name, state.fields.description, state.fields.tags],
        );
        let Ok(parsed_query) = parser.parse_query(&q) else {
            return Vec::new();
        };

        let searcher = state.reader.searcher();
        let Ok(top_docs) = searcher.search(&parsed_query, &TopDocs::with_limit(limit)) else {
            return Vec::new();
        };

        top_docs
            .into_iter()
            .filter_map(|(score, doc_address)| {
                let doc: TantivyDocument = searcher.doc(doc_address).ok()?;
                let name = doc
                    .get_first(state.fields.name)
                    .and_then(|value| value.as_str())?
                    .to_owned();
                let version = doc
                    .get_first(state.fields.version)
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_owned();
                let description = doc
                    .get_first(state.fields.description)
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_owned();
                let tags: Vec<String> = doc
                    .get_all(state.fields.tags)
                    .filter_map(|value| value.as_str())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>();

                let (matched_field, snippet) = if name.to_lowercase().contains(&q) {
                    ("name".to_owned(), name.clone())
                } else if description.to_lowercase().contains(&q) {
                    ("description".to_owned(), crop_snippet(&description, &q))
                } else if let Some(tag) = tags
                    .iter()
                    .find(|tag| tag.to_lowercase().contains(&q))
                {
                    ("tags".to_owned(), tag.clone())
                } else {
                    ("content".to_owned(), crop_snippet(&description, &q))
                };

                Some(SkillMatch {
                    name,
                    version,
                    score: score_to_rank(score),
                    snippet,
                    matched_field,
                })
            })
            .collect()
    }
}

fn score_to_rank(score: f32) -> u32 {
    if !score.is_finite() || score <= 0.0 {
        return 0;
    }
    let scaled = (score * 100.0).round();
    if scaled > u32::MAX as f32 {
        u32::MAX
    } else {
        scaled as u32
    }
}

fn crop_snippet(text: &str, query: &str) -> String {
    let lower = text.to_lowercase();
    if let Some(index) = lower.find(query) {
        let start = index.saturating_sub(16);
        let end = (index + query.len() + 16).min(text.len());
        return text[start..end].to_owned();
    }
    text.chars().take(40).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillVersionRecord {
    pub name: String,
    pub version: String,
    pub created_at_ms: u64,
    pub file_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillVersionDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SkillVersionManager {
    history_root: PathBuf,
}

impl SkillVersionManager {
    #[must_use]
    pub fn new() -> Self {
        let root = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir())
            .join(".aman")
            .join("skills")
            .join("history");
        Self { history_root: root }
    }

    #[must_use]
    pub fn from_root(path: PathBuf) -> Self {
        Self { history_root: path }
    }
}

impl Default for SkillVersionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SkillVersionManager {
    pub fn save_version(&self, name: &str, version: &str, content: &str) -> AmanResult<PathBuf> {
        let created_at_ms = now_millis();
        let skill_dir = self.history_root.join(sanitize_name(name));
        fs::create_dir_all(&skill_dir)?;
        let file_path = skill_dir.join(format!("{version}-{created_at_ms}.skill"));
        fs::write(&file_path, content)?;
        Ok(file_path)
    }

    pub fn history(&self, name: &str) -> AmanResult<Vec<SkillVersionRecord>> {
        let skill_dir = self.history_root.join(sanitize_name(name));
        if !skill_dir.exists() {
            return Ok(Vec::new());
        }

        let mut records = Vec::new();
        for entry in fs::read_dir(&skill_dir)? {
            let path = entry?.path();
            if !path.is_file() {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            let Some((version, created_at_ms)) = parse_history_file_name(file_name) else {
                continue;
            };
            records.push(SkillVersionRecord {
                name: name.to_owned(),
                version,
                created_at_ms,
                file_path: path,
            });
        }

        records.sort_by(|left, right| right.created_at_ms.cmp(&left.created_at_ms));
        Ok(records)
    }

    pub fn rollback(&self, name: &str, version: &str, destination: &Path) -> AmanResult<()> {
        let history = self.history(name)?;
        let Some(record) = history.into_iter().find(|item| item.version == version) else {
            return Err(Error::NotFound {
                name: format!("skill version {name}@{version}"),
            });
        };
        let content = fs::read_to_string(record.file_path)?;
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(destination, content)?;
        Ok(())
    }

    pub fn diff(&self, name: &str, left_version: &str, right_version: &str) -> AmanResult<SkillVersionDiff> {
        let history = self.history(name)?;
        let left = history
            .iter()
            .find(|item| item.version == left_version)
            .ok_or_else(|| Error::NotFound {
                name: format!("skill version {name}@{left_version}"),
            })?;
        let right = history
            .iter()
            .find(|item| item.version == right_version)
            .ok_or_else(|| Error::NotFound {
                name: format!("skill version {name}@{right_version}"),
            })?;

        let left_lines = fs::read_to_string(&left.file_path)?
            .lines()
            .map(ToOwned::to_owned)
            .collect::<BTreeSet<_>>();
        let right_lines = fs::read_to_string(&right.file_path)?
            .lines()
            .map(ToOwned::to_owned)
            .collect::<BTreeSet<_>>();

        let added = right_lines
            .difference(&left_lines)
            .cloned()
            .collect::<Vec<_>>();
        let removed = left_lines
            .difference(&right_lines)
            .cloned()
            .collect::<Vec<_>>();

        Ok(SkillVersionDiff { added, removed })
    }
}

pub trait RouteRefreshNotifier: Send + Sync {
    fn refresh_routes(&self) -> AmanResult<()>;
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HotReloadReport {
    pub inserted: Vec<String>,
    pub updated_same_version: Vec<String>,
    pub updated_new_version: Vec<String>,
    pub removed: Vec<String>,
    pub failed_files: Vec<PathBuf>,
}

impl HotReloadReport {
    #[must_use]
    pub fn changed(&self) -> bool {
        !(self.inserted.is_empty()
            && self.updated_same_version.is_empty()
            && self.updated_new_version.is_empty()
            && self.removed.is_empty())
    }
}

pub struct HotReloadManager {
    skills_dir: PathBuf,
    registry: Arc<SkillRegistry>,
    search: Arc<SkillSearch>,
    version_manager: Option<Arc<SkillVersionManager>>,
    debounce_ms: u64,
    notifier: Option<Arc<dyn RouteRefreshNotifier>>,
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
    receiver: Mutex<Option<Receiver<notify::Result<notify::Event>>>>,
    last_reload_at: Mutex<Option<Instant>>,
    loaded_files: Mutex<HashMap<PathBuf, String>>,
}

impl HotReloadManager {
    #[must_use]
    pub fn new(skills_dir: PathBuf, registry: Arc<SkillRegistry>, search: Arc<SkillSearch>) -> Self {
        Self {
            skills_dir,
            registry,
            search,
            version_manager: None,
            debounce_ms: 500,
            notifier: None,
            watcher: Mutex::new(None),
            receiver: Mutex::new(None),
            last_reload_at: Mutex::new(None),
            loaded_files: Mutex::new(HashMap::new()),
        }
    }

    #[must_use]
    pub fn with_debounce_ms(mut self, debounce_ms: u64) -> Self {
        self.debounce_ms = debounce_ms;
        self
    }

    #[must_use]
    pub fn with_route_notifier(mut self, notifier: Arc<dyn RouteRefreshNotifier>) -> Self {
        self.notifier = Some(notifier);
        self
    }

    #[must_use]
    pub fn with_version_manager(mut self, version_manager: Arc<SkillVersionManager>) -> Self {
        self.version_manager = Some(version_manager);
        self
    }

    pub fn start_watching(&self) -> AmanResult<()> {
        let mut watcher_slot = self.watcher.lock().expect("hot reload watcher lock");
        if watcher_slot.is_some() {
            return Ok(());
        }

        let (tx, rx) = channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = tx.send(event);
        })
        .map_err(notify_error)?;
        watcher
            .watch(&self.skills_dir, notify::RecursiveMode::Recursive)
            .map_err(notify_error)?;

        *watcher_slot = Some(watcher);
        *self.receiver.lock().expect("hot reload receiver lock") = Some(rx);
        Ok(())
    }

    pub fn stop_watching(&self) {
        *self.watcher.lock().expect("hot reload watcher lock") = None;
        *self.receiver.lock().expect("hot reload receiver lock") = None;
    }

    pub fn poll_once(&self, timeout: Duration) -> AmanResult<Option<HotReloadReport>> {
        let receiver_guard = self.receiver.lock().expect("hot reload receiver lock");
        let Some(receiver) = receiver_guard.as_ref() else {
            return Ok(None);
        };
        let event = match receiver.recv_timeout(timeout) {
            Ok(event) => event.map_err(notify_error)?,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return Ok(None),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Error::Unrecoverable {
                    message: "hot reload watcher channel disconnected".to_owned(),
                });
            }
        };
        drop(receiver_guard);

        if !is_reload_worthy_event(&event) || !self.should_reload() {
            return Ok(None);
        }

        let report = self.reload_once()?;
        if report.changed() {
            self.mark_reloaded_now();
        }
        Ok(Some(report))
    }

    pub fn reload_once(&self) -> AmanResult<HotReloadReport> {
        let files = SkillLoader::discover_skill_files(&self.skills_dir)?;
        let discovered_set: BTreeSet<PathBuf> = files.iter().cloned().collect();
        let mut report = HotReloadReport::default();
        let mut loaded_names = BTreeSet::new();
        let mut loaded_mapping = self
            .loaded_files
            .lock()
            .expect("hot reload loaded_files lock");

        for file in files {
            let content = std::fs::read_to_string(&file).ok();
            let loaded = match SkillLoader::load_from_path(&file) {
                Ok(loaded) => loaded,
                Err(_) => {
                    report.failed_files.push(file);
                    continue;
                }
            };
            let name = loaded.skill.name().to_owned();
            if let (Some(version_manager), Some(content)) = (&self.version_manager, content) {
                let version = loaded.skill.version().to_string();
                if version_manager
                    .history(&name)
                    .ok()
                    .is_none_or(|records| !records.iter().any(|record| record.version == version))
                {
                    let _ = version_manager.save_version(&name, &version, &content);
                }
            }
            loaded_names.insert(name.clone());
            let outcome = self.registry.upsert_loaded(loaded);
            let skill = self.registry.get(&name).ok_or_else(|| Error::NotFound {
                name: format!("skill:{name}"),
            })?;
            self.search.index_runtime_skill(skill.as_ref());
            loaded_mapping.insert(file, name.clone());
            match outcome {
                SkillUpsertOutcome::Inserted => report.inserted.push(name),
                SkillUpsertOutcome::ReplacedSameVersion { .. } => {
                    report.updated_same_version.push(name);
                }
                SkillUpsertOutcome::ReplacedNewVersion { .. } => {
                    report.updated_new_version.push(name);
                }
            }
        }

        let stale_files = loaded_mapping
            .keys()
            .filter(|path| !discovered_set.contains(*path))
            .cloned()
            .collect::<Vec<_>>();
        for stale_file in stale_files {
            if let Some(name) = loaded_mapping.remove(&stale_file) {
                if loaded_names.contains(&name) {
                    continue;
                }
                if self.registry.unregister(&name).is_ok() {
                    self.search.remove_skill(&name);
                    report.removed.push(name);
                }
            }
        }
        drop(loaded_mapping);

        if report.changed() {
            if let Some(notifier) = &self.notifier {
                notifier.refresh_routes()?;
            }
            self.mark_reloaded_now();
        }

        Ok(report)
    }

    fn should_reload(&self) -> bool {
        let last = self.last_reload_at.lock().expect("hot reload last_reload lock");
        match *last {
            Some(timestamp) => timestamp.elapsed().as_millis() >= u128::from(self.debounce_ms),
            None => true,
        }
    }

    fn mark_reloaded_now(&self) {
        *self.last_reload_at.lock().expect("hot reload last_reload lock") = Some(Instant::now());
    }
}

fn notify_error(error: notify::Error) -> Error {
    Error::Unrecoverable {
        message: format!("notify error: {error}"),
    }
}

fn is_reload_worthy_event(event: &notify::Event) -> bool {
    matches!(
        event.kind,
        notify::EventKind::Create(_)
            | notify::EventKind::Modify(_)
            | notify::EventKind::Remove(_)
            | notify::EventKind::Any
            | notify::EventKind::Other
    )
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

fn parse_history_file_name(file_name: &str) -> Option<(String, u64)> {
    let stem = file_name.strip_suffix(".skill")?;
    let (version, timestamp) = stem.rsplit_once('-')?;
    let created_at_ms = timestamp.parse::<u64>().ok()?;
    Some((version.to_owned(), created_at_ms))
}

fn now_millis() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

fn parse_skill_concurrency(value: Option<serde_yaml::Value>) -> AmanResult<SkillConcurrencyModel> {
    let Some(value) = value else {
        return Ok(SkillConcurrencyModel::Serial);
    };

    if let Some(mode) = value.as_str() {
        return match mode {
            "serial" => Ok(SkillConcurrencyModel::Serial),
            "parallel" => Ok(SkillConcurrencyModel::Parallel),
            _ => Err(Error::ConfigInvalid {
                message: format!("unsupported concurrency mode: {mode}"),
            }),
        };
    }

    if let Some(map) = value.as_mapping() {
        let key = serde_yaml::Value::String("limited".to_owned());
        if let Some(limit) = map.get(&key).and_then(serde_yaml::Value::as_u64) {
            let limit = usize::try_from(limit).map_err(|_| Error::ConfigInvalid {
                message: format!("invalid limited concurrency value: {limit}"),
            })?;
            return Ok(SkillConcurrencyModel::Limited(limit));
        }
    }

    Err(Error::ConfigInvalid {
        message: "invalid concurrency config, expected `serial`, `parallel`, or `{ limited: N }`"
            .to_owned(),
    })
}

fn discover_files_recursive(root: &Path, found: &mut Vec<PathBuf>) -> AmanResult<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            discover_files_recursive(&path, found)?;
            continue;
        }
        let is_yaml = matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("yaml") | Some("yml")
        );
        let is_skill_md = path.file_name().and_then(|name| name.to_str()) == Some("SKILL.md");
        if is_yaml || is_skill_md {
            found.push(path);
        }
    }
    Ok(())
}

fn extract_skill_markdown_yaml(content: &str) -> AmanResult<String> {
    let mut in_block = false;
    let mut lines = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if !in_block && (trimmed == "```yaml" || trimmed == "```yml") {
            in_block = true;
            continue;
        }
        if in_block && trimmed == "```" {
            break;
        }
        if in_block {
            lines.push(line);
        }
    }

    if lines.is_empty() {
        return Err(Error::ConfigInvalid {
            message: "SKILL.md must contain a fenced yaml block".to_owned(),
        });
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::{
        skill_matches_event, trigger_matches, HotReloadManager, IndexedSkill, RouteRefreshNotifier,
        SkillConcurrencyModel, SkillExecutor, SkillLoader, SkillRegistry, SkillSearch,
        SkillVersionManager,
    };
    use kernel::context::{BaseContext, SkillContext, ToolContext};
    use kernel::event::{Event, EventType};
    use kernel::schema::JsonSchema;
    use kernel::skill::{Skill, TriggerCondition};
    use kernel::tool::Tool;
    use kernel::types::{Priority, SourceId, ToolMode, TraceId};
    use kernel::{AmanResult, Error};
    use semver::Version;
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tool::{install_builtin_tools, ToolRegistry, ToolRunner, ToolSecurityConfig};

    struct CountingSkill {
        name: String,
        version: Version,
        triggers: Vec<TriggerCondition>,
        calls: Arc<Mutex<usize>>,
    }

    #[async_trait::async_trait]
    impl Skill for CountingSkill {
        fn name(&self) -> &str {
            &self.name
        }

        fn version(&self) -> &Version {
            &self.version
        }

        fn description(&self) -> &str {
            "test skill"
        }

        fn triggers(&self) -> &[TriggerCondition] {
            &self.triggers
        }

        async fn execute(&self, _event: Event, _ctx: SkillContext) -> AmanResult<()> {
            let mut calls = self.calls.lock().expect("calls mutex");
            *calls += 1;
            Ok(())
        }
    }

    struct ToolCallingSkill {
        version: Version,
        triggers: Vec<TriggerCondition>,
        runner: Arc<ToolRunner>,
        target_file: String,
    }

    #[async_trait::async_trait]
    impl Skill for ToolCallingSkill {
        fn name(&self) -> &str {
            "tool-caller"
        }

        fn version(&self) -> &Version {
            &self.version
        }

        fn description(&self) -> &str {
            "writes with file tool"
        }

        fn triggers(&self) -> &[TriggerCondition] {
            &self.triggers
        }

        async fn execute(&self, _event: Event, ctx: SkillContext) -> AmanResult<()> {
            let mut tool_ctx = ToolContext {
                base: ctx.base,
                tool_name: Some("write".to_owned()),
                working_directory: None,
            };
            tool_ctx.base.timeout_ms = Some(500);
            self.runner
                .execute(
                    "write",
                    json!({
                        "path": self.target_file,
                        "content": "written by skill"
                    }),
                    tool_ctx,
                )
                .await?;
            Ok(())
        }
    }

    struct NoopTool;

    #[async_trait::async_trait]
    impl Tool for NoopTool {
        fn name(&self) -> &str {
            "noop"
        }

        fn mode(&self) -> ToolMode {
            ToolMode::Local
        }

        fn parameters(&self) -> &JsonSchema {
            static PARAMS: std::sync::LazyLock<JsonSchema> =
                std::sync::LazyLock::new(|| JsonSchema::from(json!({"type": "object"})));
            &PARAMS
        }

        fn returns(&self) -> &JsonSchema {
            static RETURNS: std::sync::LazyLock<JsonSchema> =
                std::sync::LazyLock::new(|| JsonSchema::from(json!({"type": "object"})));
            &RETURNS
        }

        async fn execute(&self, _params: Value, _ctx: ToolContext) -> AmanResult<Value> {
            Ok(json!({"ok": true}))
        }
    }

    fn sample_event() -> Event {
        let mut event = Event::new(
            "webhook:billing",
            EventType::WebhookReceived,
            json!({"invoice": "inv_1"}),
        );
        event.priority = Priority::High;
        event
    }

    #[derive(Default)]
    struct CountingNotifier {
        calls: Mutex<usize>,
    }

    impl CountingNotifier {
        fn count(&self) -> usize {
            *self.calls.lock().expect("notifier mutex")
        }
    }

    impl RouteRefreshNotifier for CountingNotifier {
        fn refresh_routes(&self) -> AmanResult<()> {
            let mut calls = self.calls.lock().expect("notifier mutex");
            *calls += 1;
            Ok(())
        }
    }

    #[test]
    fn trigger_condition_matches_expected_event_fields() {
        let event = sample_event();
        let condition = TriggerCondition {
            event_types: vec![EventType::WebhookReceived],
            sources: vec![SourceId::new("webhook:billing")],
            priorities: vec![Priority::High],
            match_all: true,
        };
        assert!(trigger_matches(&condition, &event));

        let non_match = TriggerCondition {
            event_types: vec![EventType::TimerTick],
            sources: Vec::new(),
            priorities: Vec::new(),
            match_all: false,
        };
        assert!(!trigger_matches(&non_match, &event));
    }

    #[test]
    fn registry_supports_register_enable_disable() {
        let calls = Arc::new(Mutex::new(0));
        let registry = SkillRegistry::new();
        registry
            .register(Arc::new(CountingSkill {
                name: "count".to_owned(),
                version: Version::new(0, 1, 0),
                triggers: vec![TriggerCondition {
                    event_types: vec![EventType::WebhookReceived],
                    sources: Vec::new(),
                    priorities: Vec::new(),
                    match_all: false,
                }],
                calls: Arc::clone(&calls),
            }))
            .expect("register skill");
        assert!(registry.get("count").is_some());
        assert_eq!(registry.enabled_skills().len(), 1);
        assert_eq!(
            registry.model_of("count"),
            Some(SkillConcurrencyModel::Serial)
        );
        registry.disable("count").expect("disable skill");
        assert_eq!(registry.enabled_skills().len(), 0);
        registry.enable("count").expect("enable skill");
        assert_eq!(registry.enabled_skills().len(), 1);
        registry
            .set_concurrency("count", SkillConcurrencyModel::Limited(2))
            .expect("set skill concurrency");
        assert_eq!(
            registry.model_of("count"),
            Some(SkillConcurrencyModel::Limited(2))
        );
    }

    #[test]
    fn executor_runs_only_matching_enabled_skills() {
        pollster::block_on(async {
            let calls = Arc::new(Mutex::new(0));
            let registry = Arc::new(SkillRegistry::new());
            registry
                .register(Arc::new(CountingSkill {
                    name: "count".to_owned(),
                    version: Version::new(0, 1, 0),
                    triggers: vec![TriggerCondition {
                        event_types: vec![EventType::WebhookReceived],
                        sources: Vec::new(),
                        priorities: Vec::new(),
                        match_all: false,
                    }],
                    calls: Arc::clone(&calls),
                }))
                .expect("register skill");

            registry
                .register(Arc::new(CountingSkill {
                    name: "other".to_owned(),
                    version: Version::new(0, 1, 0),
                    triggers: vec![TriggerCondition {
                        event_types: vec![EventType::TimerTick],
                        sources: Vec::new(),
                        priorities: Vec::new(),
                        match_all: false,
                    }],
                    calls: Arc::new(Mutex::new(0)),
                }))
                .expect("register second skill");
            registry.disable("other").expect("disable second skill");

            let executor = SkillExecutor::new(Arc::clone(&registry));
            let result = executor
                .execute_matching(
                    sample_event(),
                    SkillContext {
                        base: BaseContext::new(TraceId::new()),
                        skill_name: None,
                        soul_name: None,
                    },
                )
                .await;

            assert_eq!(result.executed, vec!["count".to_owned()]);
            assert!(result.failed.is_empty());
            assert_eq!(*calls.lock().expect("calls mutex"), 1);
        });
    }

    #[test]
    fn skill_can_invoke_tool_runner() {
        pollster::block_on(async {
            let tool_registry = Arc::new(ToolRegistry::new());
            install_builtin_tools(&tool_registry).expect("install builtin file tool");
            tool_registry
                .register(Arc::new(NoopTool))
                .expect("register noop tool");

            let sandbox = std::env::temp_dir().join(format!("aman-skill-{}", TraceId::new()));
            std::fs::create_dir_all(&sandbox).expect("create sandbox");
            let output_file = sandbox.join("skill-output.txt");

            let runner = Arc::new(ToolRunner::new(tool_registry).with_security(ToolSecurityConfig {
                allowed_paths: vec![sandbox.clone()],
                network_allowed: false,
                command_allowlist: Vec::new(),
            }));

            let registry = Arc::new(SkillRegistry::new());
            registry
                .register(Arc::new(ToolCallingSkill {
                    version: Version::new(0, 1, 0),
                    triggers: vec![TriggerCondition {
                        event_types: vec![EventType::WebhookReceived],
                        sources: vec![SourceId::new("webhook:billing")],
                        priorities: vec![Priority::High],
                        match_all: true,
                    }],
                    runner,
                    target_file: output_file.display().to_string(),
                }))
                .expect("register tool calling skill");

            let skill = registry.get("tool-caller").expect("tool-caller exists");
            assert!(skill_matches_event(skill.as_ref(), &sample_event()));

            let executor = SkillExecutor::new(registry);
            let result = executor
                .execute_matching(
                    sample_event(),
                    SkillContext {
                        base: BaseContext::new(TraceId::new()),
                        skill_name: None,
                        soul_name: None,
                    },
                )
                .await;
            assert!(result.failed.is_empty());
            assert_eq!(result.executed, vec!["tool-caller".to_owned()]);

            let content = std::fs::read_to_string(&output_file).expect("file written by tool");
            assert_eq!(content, "written by skill");
        });
    }

    #[test]
    fn duplicate_registration_returns_error() {
        let registry = SkillRegistry::new();
        let calls = Arc::new(Mutex::new(0));
        registry
            .register(Arc::new(CountingSkill {
                name: "dup".to_owned(),
                version: Version::new(0, 1, 0),
                triggers: vec![TriggerCondition::default()],
                calls: Arc::clone(&calls),
            }))
            .expect("first register works");
        let error = registry
            .register(Arc::new(CountingSkill {
                name: "dup".to_owned(),
                version: Version::new(0, 1, 1),
                triggers: vec![TriggerCondition::default()],
                calls,
            }))
            .expect_err("duplicate should fail");
        assert!(matches!(error, Error::AlreadyExists { .. }));
    }

    #[test]
    fn skill_loader_supports_yaml_and_skill_markdown() {
        let yaml = r#"
name: invoice-review
version: 0.2.0
description: review invoice webhook
concurrency:
  limited: 2
triggers:
  - event_types: [webhook_received]
    sources: [webhook:billing]
    priorities: [high]
    match_all: true
"#;
        let loaded = SkillLoader::load_from_yaml_str(yaml).expect("yaml skill loads");
        assert_eq!(loaded.skill.name(), "invoice-review");
        assert_eq!(loaded.skill.version(), &Version::new(0, 2, 0));
        assert_eq!(loaded.concurrency, SkillConcurrencyModel::Limited(2));

        let skill_md = format!(
            "# SKILL\n\n```yaml\n{}\n```\n",
            yaml.trim()
        );
        let loaded_md =
            SkillLoader::load_from_skill_markdown_str(&skill_md).expect("markdown skill loads");
        assert_eq!(loaded_md.skill.name(), "invoice-review");
    }

    #[test]
    fn skill_loader_can_discover_skill_files() {
        let root = std::env::temp_dir().join(format!("aman-skill-discovery-{}", TraceId::new()));
        std::fs::create_dir_all(&root).expect("create root");
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).expect("create nested");

        std::fs::write(
            root.join("invoice.yaml"),
            "name: a\nversion: 0.1.0\ntriggers: []\n",
        )
        .expect("write yaml");
        std::fs::write(
            nested.join("SKILL.md"),
            "```yaml\nname: b\nversion: 0.1.0\ntriggers: []\n```\n",
        )
        .expect("write markdown");
        std::fs::write(root.join("ignore.txt"), "x").expect("write ignore");

        let files = SkillLoader::discover_skill_files(&root).expect("discover skill files");
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|path| path.ends_with("invoice.yaml")));
        assert!(files.iter().any(|path| path.ends_with("SKILL.md")));
    }

    #[test]
    fn skill_search_supports_index_search_remove() {
        let search = SkillSearch::new();
        search.index_skill(IndexedSkill {
            name: "invoice-review".to_owned(),
            version: "0.2.0".to_owned(),
            description: "review invoice and webhook payload".to_owned(),
            tags: vec!["webhook_received".to_owned(), "billing".to_owned()],
        });
        search.index_skill(IndexedSkill {
            name: "timer-cleanup".to_owned(),
            version: "0.1.0".to_owned(),
            description: "cleanup cron artifacts".to_owned(),
            tags: vec!["cron_tick".to_owned()],
        });

        let by_name = search.search("invoice", 5);
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].name, "invoice-review");
        assert_eq!(by_name[0].matched_field, "name");

        let by_tag = search.search("cron", 5);
        assert_eq!(by_tag.len(), 1);
        assert_eq!(by_tag[0].name, "timer-cleanup");

        search.remove_skill("timer-cleanup");
        let removed = search.search("timer", 5);
        assert!(removed.is_empty());
    }

    #[test]
    fn skill_version_manager_supports_history_diff_and_rollback() {
        let root = std::env::temp_dir().join(format!("aman-skill-history-{}", TraceId::new()));
        std::fs::create_dir_all(&root).expect("create history root");
        let manager = SkillVersionManager::from_root(root.clone());

        manager
            .save_version("invoice-review", "0.1.0", "name: invoice-review\nstep: a\n")
            .expect("save v1");
        std::thread::sleep(std::time::Duration::from_millis(1));
        manager
            .save_version(
                "invoice-review",
                "0.2.0",
                "name: invoice-review\nstep: a\nstep: b\n",
            )
            .expect("save v2");

        let history = manager.history("invoice-review").expect("load history");
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].version, "0.2.0");

        let diff = manager
            .diff("invoice-review", "0.1.0", "0.2.0")
            .expect("diff versions");
        assert!(diff.added.iter().any(|line| line == "step: b"));
        assert!(diff.removed.is_empty());

        let rollback_path = root.join("rollback").join("invoice.yaml");
        manager
            .rollback("invoice-review", "0.1.0", &rollback_path)
            .expect("rollback v1");
        let rolled = std::fs::read_to_string(&rollback_path).expect("read rollback file");
        assert!(rolled.contains("step: a"));
        assert!(!rolled.contains("step: b"));
    }

    #[test]
    fn hot_reload_once_updates_registry_search_and_notifier() {
        let root = std::env::temp_dir().join(format!("aman-hot-reload-{}", TraceId::new()));
        std::fs::create_dir_all(&root).expect("create hot reload root");
        std::fs::write(
            root.join("invoice.yaml"),
            "name: invoice-hot\nversion: 0.1.0\ndescription: hot reload test\ntriggers: []\n",
        )
        .expect("write skill yaml");

        let registry = Arc::new(SkillRegistry::new());
        let search = Arc::new(SkillSearch::new());
        let notifier = Arc::new(CountingNotifier::default());
        let notifier_dyn: Arc<dyn RouteRefreshNotifier> = notifier.clone();
        let manager = HotReloadManager::new(root, Arc::clone(&registry), Arc::clone(&search))
            .with_route_notifier(notifier_dyn);

        let report = manager.reload_once().expect("reload once succeeds");
        assert!(report.changed());
        assert!(report.inserted.iter().any(|name| name == "invoice-hot"));
        assert!(registry.get("invoice-hot").is_some());
        assert_eq!(search.search("invoice-hot", 5).len(), 1);
        assert_eq!(notifier.count(), 1);
    }

    #[test]
    fn hot_reload_poll_once_uses_notify_and_debounce() {
        let root = std::env::temp_dir().join(format!("aman-hot-watch-{}", TraceId::new()));
        std::fs::create_dir_all(&root).expect("create watcher root");

        let registry = Arc::new(SkillRegistry::new());
        let search = Arc::new(SkillSearch::new());
        let manager = HotReloadManager::new(root.clone(), Arc::clone(&registry), Arc::clone(&search))
            .with_debounce_ms(500);
        manager.start_watching().expect("start watching");

        std::fs::write(
            root.join("watch.yaml"),
            "name: watch-skill\nversion: 0.1.0\ndescription: watcher\ntriggers: []\n",
        )
        .expect("write watched file");

        let first = manager
            .poll_once(Duration::from_secs(2))
            .expect("poll should succeed");
        assert!(first.is_some());
        assert!(registry.get("watch-skill").is_some());

        std::fs::write(
            root.join("watch.yaml"),
            "name: watch-skill\nversion: 0.1.0\ndescription: watcher updated\ntriggers: []\n",
        )
        .expect("rewrite watched file");
        let second = manager
            .poll_once(Duration::from_millis(800))
            .expect("second poll should succeed");
        if let Some(report) = second {
            assert!(!report.changed(), "second poll within debounce should not reload");
        }

        std::thread::sleep(Duration::from_millis(550));
        std::fs::write(
            root.join("watch.yaml"),
            "name: watch-skill\nversion: 0.2.0\ndescription: watcher updated\ntriggers: []\n",
        )
        .expect("rewrite watched file after debounce");
        let third = manager
            .poll_once(Duration::from_secs(2))
            .expect("third poll should succeed");
        assert!(third.is_some());
        manager.stop_watching();
    }

    #[test]
    fn hot_reload_reports_same_and_new_version_updates() {
        let root = std::env::temp_dir().join(format!("aman-hot-version-{}", TraceId::new()));
        std::fs::create_dir_all(&root).expect("create hot reload root");
        let skill_file = root.join("versioned.yaml");
        std::fs::write(
            &skill_file,
            "name: versioned\nversion: 0.1.0\ndescription: first\ntriggers: []\n",
        )
        .expect("write initial skill");

        let registry = Arc::new(SkillRegistry::new());
        let search = Arc::new(SkillSearch::new());
        let manager = HotReloadManager::new(root, Arc::clone(&registry), Arc::clone(&search));

        let first = manager.reload_once().expect("first reload");
        assert_eq!(first.inserted, vec!["versioned".to_owned()]);

        std::fs::write(
            &skill_file,
            "name: versioned\nversion: 0.1.0\ndescription: same version changed\ntriggers: []\n",
        )
        .expect("rewrite same version");
        let second = manager.reload_once().expect("second reload");
        assert_eq!(second.updated_same_version, vec!["versioned".to_owned()]);
        assert!(second.updated_new_version.is_empty());

        std::fs::write(
            &skill_file,
            "name: versioned\nversion: 0.2.0\ndescription: new version changed\ntriggers: []\n",
        )
        .expect("rewrite new version");
        let third = manager.reload_once().expect("third reload");
        assert_eq!(third.updated_new_version, vec!["versioned".to_owned()]);
        assert!(third.updated_same_version.is_empty());
    }

    #[test]
    fn hot_reload_invalid_files_do_not_trigger_route_refresh() {
        let root = std::env::temp_dir().join(format!("aman-hot-invalid-{}", TraceId::new()));
        std::fs::create_dir_all(&root).expect("create hot reload root");
        std::fs::write(root.join("broken.yaml"), "name: bad\nversion: invalid\n")
            .expect("write broken skill");

        let registry = Arc::new(SkillRegistry::new());
        let search = Arc::new(SkillSearch::new());
        let notifier = Arc::new(CountingNotifier::default());
        let notifier_dyn: Arc<dyn RouteRefreshNotifier> = notifier.clone();
        let manager = HotReloadManager::new(root, Arc::clone(&registry), Arc::clone(&search))
            .with_route_notifier(notifier_dyn);

        let report = manager.reload_once().expect("reload should finish");
        assert!(!report.changed());
        assert_eq!(report.failed_files.len(), 1);
        assert_eq!(notifier.count(), 0);
        assert!(registry.enabled_skills().is_empty());
    }

    #[test]
    fn hot_reload_removes_deleted_skill_and_refreshes_routes() {
        let root = std::env::temp_dir().join(format!("aman-hot-remove-{}", TraceId::new()));
        std::fs::create_dir_all(&root).expect("create hot reload root");
        let skill_path = root.join("remove-me.yaml");
        std::fs::write(
            &skill_path,
            "name: remove-me\nversion: 0.1.0\ndescription: removable\ntriggers: []\n",
        )
        .expect("write removable skill");

        let registry = Arc::new(SkillRegistry::new());
        let search = Arc::new(SkillSearch::new());
        let notifier = Arc::new(CountingNotifier::default());
        let notifier_dyn: Arc<dyn RouteRefreshNotifier> = notifier.clone();
        let manager = HotReloadManager::new(root, Arc::clone(&registry), Arc::clone(&search))
            .with_route_notifier(notifier_dyn);

        let first = manager.reload_once().expect("initial reload");
        assert_eq!(first.inserted, vec!["remove-me".to_owned()]);
        assert_eq!(notifier.count(), 1);
        assert!(registry.get("remove-me").is_some());
        assert_eq!(search.search("remove-me", 5).len(), 1);

        std::fs::remove_file(skill_path).expect("delete skill file");
        let second = manager.reload_once().expect("reload after deletion");
        assert_eq!(second.removed, vec!["remove-me".to_owned()]);
        assert!(registry.get("remove-me").is_none());
        assert!(search.search("remove-me", 5).is_empty());
        assert_eq!(notifier.count(), 2);
    }

    #[test]
    fn hot_reload_recovers_when_invalid_file_becomes_valid() {
        let root = std::env::temp_dir().join(format!("aman-hot-recover-{}", TraceId::new()));
        std::fs::create_dir_all(&root).expect("create hot reload root");
        let skill_file = root.join("recover.yaml");
        std::fs::write(&skill_file, "name: recover\nversion: invalid\n")
            .expect("write invalid skill");

        let registry = Arc::new(SkillRegistry::new());
        let search = Arc::new(SkillSearch::new());
        let notifier = Arc::new(CountingNotifier::default());
        let notifier_dyn: Arc<dyn RouteRefreshNotifier> = notifier.clone();
        let manager = HotReloadManager::new(root, Arc::clone(&registry), Arc::clone(&search))
            .with_route_notifier(notifier_dyn);

        let first = manager.reload_once().expect("reload invalid file");
        assert!(!first.changed());
        assert_eq!(first.failed_files.len(), 1);
        assert!(registry.get("recover").is_none());
        assert!(search.search("recover", 5).is_empty());
        assert_eq!(notifier.count(), 0);

        std::fs::write(
            &skill_file,
            "name: recover\nversion: 0.1.0\ndescription: fixed\ntriggers: []\n",
        )
        .expect("rewrite valid skill");
        let second = manager.reload_once().expect("reload fixed file");
        assert_eq!(second.inserted, vec!["recover".to_owned()]);
        assert!(second.failed_files.is_empty());
        assert!(registry.get("recover").is_some());
        assert_eq!(search.search("recover", 5).len(), 1);
        assert_eq!(notifier.count(), 1);
    }

    #[test]
    fn hot_reload_applies_latest_content_after_rapid_rewrites() {
        let root = std::env::temp_dir().join(format!("aman-hot-jitter-{}", TraceId::new()));
        std::fs::create_dir_all(&root).expect("create hot reload root");
        let skill_file = root.join("jitter.yaml");
        std::fs::write(
            &skill_file,
            "name: jitter\nversion: 0.1.0\ndescription: base\ntriggers: []\n",
        )
        .expect("write initial skill");

        let registry = Arc::new(SkillRegistry::new());
        let search = Arc::new(SkillSearch::new());
        let notifier = Arc::new(CountingNotifier::default());
        let notifier_dyn: Arc<dyn RouteRefreshNotifier> = notifier.clone();
        let manager = HotReloadManager::new(root, Arc::clone(&registry), Arc::clone(&search))
            .with_route_notifier(notifier_dyn);

        let first = manager.reload_once().expect("first reload");
        assert_eq!(first.inserted, vec!["jitter".to_owned()]);
        assert_eq!(notifier.count(), 1);

        std::fs::write(
            &skill_file,
            "name: jitter\nversion: 0.2.0\ndescription: rewrite-1\ntriggers: []\n",
        )
        .expect("rewrite-1");
        std::fs::write(
            &skill_file,
            "name: jitter\nversion: 0.3.0\ndescription: rewrite-2\ntriggers: []\n",
        )
        .expect("rewrite-2");
        std::fs::write(
            &skill_file,
            "name: jitter\nversion: 0.4.0\ndescription: rewrite-3\ntriggers: []\n",
        )
        .expect("rewrite-3");

        let second = manager.reload_once().expect("second reload");
        assert_eq!(second.updated_new_version, vec!["jitter".to_owned()]);
        assert!(second.updated_same_version.is_empty());
        let loaded = registry.get("jitter").expect("jitter skill exists");
        assert_eq!(loaded.version(), &Version::new(0, 4, 0));
        assert!(search.search("rewrite-3", 5).iter().any(|m| m.name == "jitter"));
        assert_eq!(notifier.count(), 2);
    }

    #[test]
    fn hot_reload_watches_skill_markdown_and_applies_update() {
        let root = std::env::temp_dir().join(format!("aman-hot-skill-md-{}", TraceId::new()));
        let skill_dir = root.join("invoice");
        std::fs::create_dir_all(&skill_dir).expect("create skill markdown dir");
        let skill_file = skill_dir.join("SKILL.md");

        let registry = Arc::new(SkillRegistry::new());
        let search = Arc::new(SkillSearch::new());
        let manager = HotReloadManager::new(root.clone(), Arc::clone(&registry), Arc::clone(&search))
            .with_debounce_ms(300);
        manager.start_watching().expect("start watcher");

        std::fs::write(
            &skill_file,
            r#"# Invoice Skill

```yaml
name: invoice-md
version: 0.1.0
description: markdown hot reload
triggers: []
```
"#,
        )
        .expect("write initial skill markdown");

        let first = manager
            .poll_once(Duration::from_secs(2))
            .expect("first poll should succeed");
        assert!(first.is_some(), "first poll should detect markdown skill");
        let initial = registry.get("invoice-md").expect("skill should be loaded");
        assert_eq!(initial.version(), &Version::new(0, 1, 0));
        assert_eq!(search.search("markdown hot reload", 5).len(), 1);

        std::thread::sleep(Duration::from_millis(350));
        std::fs::write(
            &skill_file,
            r#"# Invoice Skill

```yaml
name: invoice-md
version: 0.2.0
description: markdown updated
triggers: []
```
"#,
        )
        .expect("rewrite skill markdown");

        let second = manager
            .poll_once(Duration::from_secs(2))
            .expect("second poll should succeed");
        let report = second.expect("second poll should produce report");
        assert_eq!(report.updated_new_version, vec!["invoice-md".to_owned()]);
        let updated = registry.get("invoice-md").expect("updated skill should exist");
        assert_eq!(updated.version(), &Version::new(0, 2, 0));
        assert_eq!(search.search("markdown updated", 5).len(), 1);
        manager.stop_watching();
    }
}
