use super::App;
use crate::git::{self, github, Checks};
use serde_json::{json, Value};
use std::path::Path;

fn checks(check: Checks) -> &'static str {
    match check {
        Checks::None => "none",
        Checks::Pending => "pending",
        Checks::Passing => "passing",
        Checks::Failing => "failing",
    }
}
fn short(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}
fn fetch(cwd: &Path, number: Option<u32>) -> Result<Value, String> {
    if let Some(note) = github::detect().note() {
        return Err(note.into());
    }
    git::local::origin_slug(cwd).ok_or("workspace has no GitHub origin")?;
    if let Some(number) = number {
        let pr = github::pr_detail(cwd, u64::from(number))?;
        return Ok(
            json!({"number":pr.number,"title":short(&pr.title,512),"state":pr.state,"author":pr.author,
            "base":pr.base,"head":pr.head,"body":short(&pr.body,32768),"review":pr.review_decision,
            "additions":pr.additions,"deletions":pr.deletions,"checks":pr.check_runs.iter().take(128).map(|c|
                json!({"name":short(&c.name,256),"status":checks(c.bucket)})).collect::<Vec<_>>() }),
        );
    }
    Ok(
        json!({"items":github::pull_requests(cwd,git::Scope::ThisRepo,git::StateFilter::Open)?
        .iter().take(50).map(|pr|json!({"number":pr.number,"title":short(&pr.title,512),"author":short(&pr.author,128),"checks":checks(pr.checks),"draft":pr.is_draft})).collect::<Vec<_>>() }),
    )
}
impl App {
    pub(crate) fn desktop_logs(
        &mut self,
        client: bool,
        done: impl FnOnce(Result<Value, String>) + Send + 'static,
    ) -> Result<(), &'static str> {
        self.io_jobs.submit(self.app_tx.clone(), move || {
            let result = crate::logging::desktop_records(client).map_err(|e| e.to_string());
            Box::new(move |_| {
                done(result);
                false
            })
        })
    }
    pub(crate) fn desktop_pull_requests(
        &mut self,
        ws: String,
        number: Option<u32>,
        done: impl FnOnce(Result<Value, String>) + Send + 'static,
    ) -> Result<(), &'static str> {
        let workspace = self
            .workspaces
            .get(self.active_ws)
            .filter(|w| w.id == ws && w.remote.is_none())
            .ok_or("workspace changed")?;
        let cwd = workspace.cwd.clone();
        self.io_jobs.submit(self.app_tx.clone(), move || {
            let data = fetch(&cwd, number);
            Box::new(move |app| {
                if app
                    .workspaces
                    .get(app.active_ws)
                    .is_some_and(|w| w.id == ws && w.cwd == cwd)
                {
                    done(data);
                } else {
                    done(Err("workspace changed".into()));
                }
                false
            })
        })
    }
}
