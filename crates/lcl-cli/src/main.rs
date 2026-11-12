use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use lcl_core::db::Store;
use lcl_core::models::*;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "lcl", version, about = "Local Linear-like tracker (SQLite per project)")]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, global = true, env = "LCL_PROJECT")]
    project: Option<String>,
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Init { name: Option<String> },
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    Issue {
        #[command(subcommand)]
        cmd: IssueCmd,
    },
    Doc {
        #[command(subcommand)]
        cmd: DocCmd,
    },
    Label {
        #[command(subcommand)]
        cmd: LabelCmd,
    },
    Cycle {
        #[command(subcommand)]
        cmd: CycleCmd,
    },
    Search { query: String },
    Tui,
}

#[derive(Subcommand)]
enum ProjectCmd {
    Create { name: String, #[arg(long)] key: Option<String>, #[arg(long)] global: bool },
    List,
    Show,
}

#[derive(Subcommand)]
enum IssueCmd {
    Create {
        #[arg(long)] title: String,
        #[arg(long, default_value = "")] description: String,
        #[arg(long, default_value = "Backlog")] status: String,
        #[arg(long, default_value_t = 0)] priority: i64,
        #[arg(long)] assignee: Option<String>,
        #[arg(long)] parent: Option<String>,
        #[arg(long)] label: Vec<String>,
        #[arg(long)] estimate: Option<i64>,
        #[arg(long)] due: Option<String>,
    },
    List {
        #[arg(long)] status: Option<String>,
        #[arg(long)] assignee: Option<String>,
        #[arg(long, default_value_t = 50)] limit: i64,
    },
    Show { id: String },
    Update {
        id: String,
        #[arg(long)] title: Option<String>,
        #[arg(long)] description: Option<String>,
        #[arg(long)] status: Option<String>,
        #[arg(long)] priority: Option<i64>,
        #[arg(long)] assignee: Option<String>,
        #[arg(long)] estimate: Option<i64>,
        #[arg(long)] due: Option<String>,
        #[arg(long)] label: Vec<String>,
    },
    Delete { id: String },
    Comment { id: String, body: String, #[arg(long)] author: Option<String> },
    Comments { id: String },
}

#[derive(Subcommand)]
enum DocCmd {
    Create { #[arg(long)] title: String, #[arg(long, default_value = "")] content: String, #[arg(long)] parent: Option<String> },
    List,
    Show { id: String },
    Update { id: String, #[arg(long)] title: Option<String>, #[arg(long)] content: Option<String> },
    Delete { id: String },
}

#[derive(Subcommand)]
enum LabelCmd {
    Create { name: String, #[arg(long, default_value = "#888888")] color: String },
    List,
    Delete { id: String },
}

#[derive(Subcommand)]
enum CycleCmd {
    Create { name: String, #[arg(long)] start: Option<String>, #[arg(long)] end: Option<String> },
    List,
}

fn global_dir() -> PathBuf { dirs_home().join(".lcl") }
fn dirs_home() -> PathBuf { std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/tmp")) }
fn projects_dir() -> PathBuf { global_dir().join("projects") }
fn index_db_path() -> PathBuf { global_dir().join("index.sqlite") }

fn open_index() -> Result<Store> {
    let p = index_db_path();
    let s = Store::open(&p)?;
    // ensure projects table exists (index-specific)
    s.conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS projects (name TEXT PRIMARY KEY, slug TEXT NOT NULL, path TEXT NOT NULL, created_at INTEGER NOT NULL);"
    )?;
    Ok(s)
}

fn slugify(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect::<String>().trim_matches('-').to_string()
}

fn find_local_db() -> Option<PathBuf> {
    let mut cur = std::env::current_dir().ok()?;
    loop {
        let cand = cur.join(".lcl").join("data.sqlite");
        if cand.exists() { return Some(cand); }
        if !cur.pop() { break; }
    }
    None
}

fn resolve_store(cli_project: &Option<String>) -> Result<Store> {
    if let Some(name) = cli_project {
        let slug = slugify(name);
        let p = projects_dir().join(format!("{}.sqlite", slug));
        if !p.exists() {
            return Err(anyhow::anyhow!("project '{}' not found at {}", name, p.display()));
        }
        return Store::open(&p).context("open project db");
    }
    if let Some(p) = find_local_db() {
        return Store::open(&p).context("open local db");
    }
    // global fallback: if index has projects, try default? require explicit
    Err(anyhow::anyhow!("no project found. Run `lcl init` or pass --project <name>"))
}

fn print_json<T: serde::Serialize>(v: &T) {
    println!("{}", serde_json::to_string_pretty(v).unwrap());
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Commands::Init { name } => {
            let pname = name.unwrap_or_else(|| {
                std::env::current_dir().ok().and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string())).unwrap_or_else(|| "project".into())
            });
            let key = pname.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_uppercase();
            let key = if key.is_empty() { "PRJ".to_string() } else { key.chars().take(4).collect() };
            let dir = std::env::current_dir()?.join(".lcl");
            std::fs::create_dir_all(&dir)?;
            let dbpath = dir.join("data.sqlite");
            let store = Store::open(&dbpath)?;
            store.set_meta("name", &pname)?;
            store.set_meta("key", &key)?;
            store.set_meta("description", "")?;
            store.set_meta("created_at", &now().to_string())?;
            // register in global index
            let idx = open_index()?;
            idx.conn.execute("INSERT OR REPLACE INTO projects(name,slug,path,created_at) VALUES(?1,?2,?3,?4)", rusqlite::params![pname, slugify(&pname), dbpath.to_string_lossy(), now()])?;
            if cli.json { print_json(&serde_json::json!({"path": dbpath, "name": pname, "key": key})); }
            else { println!("Initialized project '{}' at {}", pname, dbpath.display()); }
        }
        Commands::Project { cmd } => match cmd {
            ProjectCmd::Create { name, key, global } => {
                let slug = slugify(&name);
                let k = key.unwrap_or_else(|| name.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_uppercase().chars().take(4).collect());
                let k = if k.is_empty() { "PRJ".to_string() } else { k };
                let path = if global {
                    std::fs::create_dir_all(projects_dir())?;
                    projects_dir().join(format!("{}.sqlite", slug))
                } else {
                    let dir = std::env::current_dir()?.join(".lcl");
                    std::fs::create_dir_all(&dir)?;
                    dir.join(format!("{}.sqlite", slug))
                };
                let store = Store::open(&path)?;
                store.set_meta("name", &name)?;
                store.set_meta("key", &k)?;
                store.set_meta("description", "")?;
                store.set_meta("created_at", &now().to_string())?;
                let idx = open_index()?;
                idx.conn.execute("INSERT OR REPLACE INTO projects(name,slug,path,created_at) VALUES(?1,?2,?3,?4)", rusqlite::params![name, slug, path.to_string_lossy(), now()])?;
                if cli.json { print_json(&serde_json::json!({"path": path, "name": name, "key": k})); }
                else { println!("Created project '{}' at {}", name, path.display()); }
            }
            ProjectCmd::List => {
                let idx = open_index()?;
                let mut stmt = idx.conn.prepare("SELECT name,slug,path,created_at FROM projects ORDER BY name")?;
                let rows = stmt.query_map([], |r| Ok(serde_json::json!({"name": r.get::<_,String>(0)?, "slug": r.get::<_,String>(1)?, "path": r.get::<_,String>(2)?, "created_at": r.get::<_,i64>(3)?})))?;
                let mut v = vec![];
                for r in rows { v.push(r?); }
                if cli.json { print_json(&v); }
                else { for e in &v { println!("{} ({})", e["name"], e["slug"]); } }
            }
            ProjectCmd::Show => {
                let store = resolve_store(&cli.project)?;
                let name = store.get_meta("name")?.unwrap_or_default();
                let key = store.get_meta("key")?.unwrap_or_default();
                let desc = store.get_meta("description")?.unwrap_or_default();
                let created = store.get_meta("created_at")?.unwrap_or_default();
                let v = serde_json::json!({"name": name, "key": key, "description": desc, "created_at": created});
                if cli.json { print_json(&v); } else { println!("{} [{}] - {}", name, key, desc); }
            }
        },
        Commands::Issue { cmd } => {
            let store = resolve_store(&cli.project)?;
            match cmd {
                IssueCmd::Create { title, description, status, priority, assignee, parent, label, estimate, due } => {
                    let st = Status::parse(&status).unwrap_or(Status::Backlog);
                    let pr = Priority::from_i64(priority);
                    let issue = store.create_issue(Issue {
                        id: String::new(), number: 0, title, description, status: st, priority: pr,
                        assignee, parent_id: parent, estimate, due_date: due, labels: label,
                        created_at: 0, updated_at: 0, completed_at: None,
                    })?;
                    if cli.json { print_json(&issue); } else { println!("Created {}-{}", store.get_meta("key").ok().flatten().unwrap_or_default(), issue.number); }
                }
                IssueCmd::List { status, assignee, limit } => {
                    let st = status.as_ref().and_then(|s| Status::parse(s));
                    let issues = store.list_issues(st, assignee.as_deref(), limit)?;
                    if cli.json { print_json(&issues); }
                    else {
                        let key = store.get_meta("key").ok().flatten().unwrap_or_default();
                        for i in &issues { println!("{}-{}\t[{}]\t{}\t{}", key, i.number, i.status, i.priority.label(), i.title); }
                    }
                }
                IssueCmd::Show { id } => {
                    let issue = store.get_issue(&id)?.ok_or_else(|| anyhow::anyhow!("issue not found"))?;
                    let comments = store.list_comments(&issue.id)?;
                    if cli.json { print_json(&serde_json::json!({"issue": issue, "comments": comments})); }
                    else {
                        let key = store.get_meta("key").ok().flatten().unwrap_or_default();
                        println!("{}-{} {}", key, issue.number, issue.title);
                        println!("Status: {}  Priority: {}  Assignee: {:?}", issue.status, issue.priority.label(), issue.assignee);
                        if !issue.description.is_empty() { println!("\n{}", issue.description); }
                        if !comments.is_empty() {
                            println!("\nComments:");
                            for c in comments { println!("  - {}: {}", c.author.as_deref().unwrap_or("anon"), c.body); }
                        }
                    }
                }
                IssueCmd::Update { id, title, description, status, priority, assignee, estimate, due, label } => {
                    let issue = store.update_issue(&id, |i| {
                        if let Some(t) = title { i.title = t; }
                        if let Some(d) = description { i.description = d; }
                        if let Some(s) = status && let Some(st) = Status::parse(&s) { i.status = st; }
                        if let Some(p) = priority { i.priority = Priority::from_i64(p); }
                        if assignee.is_some() { i.assignee = assignee; }
                        if let Some(e) = estimate { i.estimate = Some(e); }
                        if let Some(d) = due { i.due_date = Some(d); }
                        if !label.is_empty() { i.labels = label; }
                    })?;
                    if cli.json { print_json(&issue); } else { println!("Updated {}", id); }
                }
                IssueCmd::Delete { id } => {
                    store.delete_issue(&id)?;
                    if cli.json { print_json(&serde_json::json!({"deleted": id})); } else { println!("Deleted {}", id); }
                }
                IssueCmd::Comment { id, body, author } => {
                    let issue = store.get_issue(&id)?.ok_or_else(|| anyhow::anyhow!("issue not found"))?;
                    let c = store.add_comment(Comment { id: String::new(), issue_id: issue.id, body, author, created_at: 0 })?;
                    if cli.json { print_json(&c); } else { println!("Comment added"); }
                }
                IssueCmd::Comments { id } => {
                    let issue = store.get_issue(&id)?.ok_or_else(|| anyhow::anyhow!("issue not found"))?;
                    let comments = store.list_comments(&issue.id)?;
                    if cli.json { print_json(&comments); }
                    else { for c in comments { println!("[{}] {}: {}", c.created_at, c.author.as_deref().unwrap_or("anon"), c.body); } }
                }
            }
        },
        Commands::Doc { cmd } => {
            let store = resolve_store(&cli.project)?;
            match cmd {
                DocCmd::Create { title, content, parent } => {
                    let d = store.create_doc(Doc { id: String::new(), title, content, parent_id: parent, created_at: 0, updated_at: 0 })?;
                    if cli.json { print_json(&d); } else { println!("Created doc {}", d.id); }
                }
                DocCmd::List => {
                    let docs = store.list_docs()?;
                    if cli.json { print_json(&docs); }
                    else { for d in docs { println!("{}\t{}", d.id, d.title); } }
                }
                DocCmd::Show { id } => {
                    let d = store.get_doc(&id)?.ok_or_else(|| anyhow::anyhow!("doc not found"))?;
                    if cli.json { print_json(&d); } else { println!("# {}\n\n{}", d.title, d.content); }
                }
                DocCmd::Update { id, title, content } => {
                    let d = store.update_doc(&id, title, content)?;
                    if cli.json { print_json(&d); } else { println!("Updated {}", id); }
                }
                DocCmd::Delete { id } => {
                    store.delete_doc(&id)?;
                    if cli.json { print_json(&serde_json::json!({"deleted": id})); } else { println!("Deleted {}", id); }
                }
            }
        },
        Commands::Label { cmd } => {
            let store = resolve_store(&cli.project)?;
            match cmd {
                LabelCmd::Create { name, color } => {
                    let l = Label { id: uuid::Uuid::new_v4().to_string(), name, color };
                    store.create_label(&l)?;
                    if cli.json { print_json(&l); } else { println!("Created label {}", l.name); }
                }
                LabelCmd::List => {
                    let ls = store.list_labels()?;
                    if cli.json { print_json(&ls); } else { for l in ls { println!("{} {} ({})", l.id, l.name, l.color); } }
                }
                LabelCmd::Delete { id } => {
                    store.delete_label(&id)?;
                    if cli.json { print_json(&serde_json::json!({"deleted": id})); } else { println!("Deleted {}", id); }
                }
            }
        },
        Commands::Cycle { cmd } => {
            let store = resolve_store(&cli.project)?;
            match cmd {
                CycleCmd::Create { name, start, end } => {
                    let c = store.create_cycle(Cycle { id: String::new(), name, starts_at: start, ends_at: end, created_at: 0 })?;
                    if cli.json { print_json(&c); } else { println!("Created cycle {}", c.id); }
                }
                CycleCmd::List => {
                    let cs = store.list_cycles()?;
                    if cli.json { print_json(&cs); } else { for c in cs { println!("{}\t{}\t{:?}..{:?}", c.id, c.name, c.starts_at, c.ends_at); } }
                }
            }
        },
        Commands::Search { query } => {
            let store = resolve_store(&cli.project)?;
            let issues = store.search_issues(&query)?;
            let docs = store.search_docs(&query)?;
            if cli.json { print_json(&serde_json::json!({"issues": issues, "docs": docs})); }
            else {
                println!("Issues:");
                let key = store.get_meta("key").ok().flatten().unwrap_or_default();
                for i in &issues { println!("  {}-{} {}", key, i.number, i.title); }
                println!("Docs:");
                for d in &docs { println!("  {} {}", d.id, d.title); }
            }
        },
        Commands::Tui => {
            // launch tui crate
            lcl_tui::run(cli.project.clone())?;
        }
    }
    Ok(())
}

fn now() -> i64 { lcl_core::models::now() }

// helper to avoid import name clash
