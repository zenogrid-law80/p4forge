//! Convert the requested root repository configuration without executing it.
use anyhow::{Context, Result, ensure};
use p4forge_domain::project_config::ProjectConfiguration;
use toml::Value;

fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(args.len() == 2, "usage: convert_root INPUT OUTPUT");
    let mut value: Value = toml::from_str(&std::fs::read_to_string(&args[0])?)?;
    let table = value.as_table_mut().context("expected project table")?;
    table.insert("schema_version".into(), Value::Integer(1));
    for pipeline in table
        .get_mut("pipelines")
        .and_then(Value::as_array_mut)
        .context("missing pipelines")?
    {
        let pipeline = pipeline.as_table_mut().context("pipeline table")?;
        let os = pipeline
            .get("runner_os")
            .and_then(Value::as_str)
            .context("runner_os")?;
        pipeline.insert(
            "runner_arch".into(),
            Value::String(if os == "windows" { "x86_64" } else { "aarch64" }.into()),
        );
        pipeline.insert("workspace".into(), Value::String("pipeline".into()));
        pipeline
            .entry("max_parallel_jobs")
            .or_insert(Value::Integer(1));
        if let Some(view) = pipeline.remove("sparse_view") {
            pipeline.insert("checkout_view".into(), view);
        }
        if let Some(paths) = pipeline.get_mut("changes").and_then(Value::as_array_mut) {
            // CI configuration now lives in PostgreSQL, not a VCS file.
            paths.retain(|path| path.as_str() != Some(".lore-ci.toml"));
        }
        for job in pipeline
            .get_mut("jobs")
            .and_then(Value::as_array_mut)
            .context("jobs")?
        {
            let job = job.as_table_mut().context("job")?;
            let name = job
                .get("name")
                .and_then(Value::as_str)
                .context("job name")?
                .to_owned();
            let scripts = job
                .get_mut("script")
                .and_then(Value::as_array_mut)
                .context("script")?;
            match name.as_str() {
                "data-table-generate" => {
                    scripts.retain(|script| !script.as_str().unwrap_or_default().contains("LORE_"));
                }
                "publish-generated-data" => {
                    *scripts = vec![Value::String("python3 \"${P4FORGE_PUBLISH_TOOL:?set the installed p4-publish.py path}\" GameDesign/Tools/data_table/data_table/src/... GameDesign/Output/Sqlite/...".into())];
                }
                "publish-windows-executables" => {
                    *scripts = vec![Value::String("if (-not $env:P4FORGE_PUBLISH_TOOL) { throw 'Set P4FORGE_PUBLISH_TOOL to the installed p4-publish.py path' }; python $env:P4FORGE_PUBLISH_TOOL GameDesign/LocalServer/bin/...; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }".into())];
                }
                _ => {
                    for script in scripts {
                        let text = script
                            .as_str()
                            .context("script string")?
                            .replace("LORE_REVISION", "P4FORGE_CHANGELIST");
                        ensure!(!text.contains("LORE_"), "unmapped Lore operation in {name}");
                        *script = Value::String(text);
                    }
                }
            }
        }
    }
    let config: ProjectConfiguration = value.try_into()?;
    config.validate()?;
    let output = toml::to_string_pretty(&config)?;
    ensure!(
        !output.contains("LORE_") && !output.contains(".lore-ci.toml"),
        "unconverted Lore reference"
    );
    std::fs::write(&args[1], output)?;
    println!(
        "Converted {} pipelines / {} jobs",
        config.pipelines.len(),
        config.pipelines.iter().map(|p| p.jobs.len()).sum::<usize>()
    );
    Ok(())
}
