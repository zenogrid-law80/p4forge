use anyhow::{Result, ensure};
use p4forge_perforce::{Connection, Perforce, Root};
use uuid::Uuid;

/// Uses the existing read-only //streams/root fixture imported at CL 6.
/// Only temporary client specs and a temporary local checkout are created.
#[tokio::test]
#[ignore = "requires P4FORGE_TEST_P4PORT and P4FORGE_TEST_P4_TICKETS_FILE"]
async fn real_view_checkout_remaps_server_and_excludes_client() -> Result<()> {
    let mut p4 = Perforce::new(std::env::var("P4_BIN").unwrap_or_else(|_| "p4".into()));
    p4.tickets_file = Some(std::env::var("P4FORGE_TEST_P4_TICKETS_FILE")?.into());
    let connection = Connection {
        port: std::env::var("P4FORGE_TEST_P4PORT")?,
        user: "p4forge".into(),
        charset: Some("utf8".into()),
    };
    for (included, manifest) in [
        ("//streams/root/Server/... ...", "Cargo.toml"),
        ("//streams/root/... ...", "Server/Cargo.toml"),
        (
            "//streams/root/... checkout/...",
            "checkout/Server/Cargo.toml",
        ),
        (
            "//streams/root/... \"build tree/...\"",
            "build tree/Server/Cargo.toml",
        ),
    ] {
        let root = Root::View {
            view: vec![included.into(), "-//streams/root/Client/...".into()],
        };
        let snapshot = p4.snapshot(&connection, &root, 6).await?;
        let attempt = Uuid::new_v4();
        let work = tempfile::tempdir()?;
        let result = async {
            p4.checkout(&snapshot, attempt, work.path()).await?;
            ensure!(
                work.path().join(manifest).is_file(),
                "Server mapping did not reach {manifest}"
            );
            ensure!(
                !work.path().join("Client").exists()
                    && !work.path().join("checkout/Client").exists()
                    && !work.path().join("build tree/Client").exists(),
                "excluded Client files reached the workspace"
            );
            ensure!(
                !work.path().join("root0").exists(),
                "new View must not use legacy root0 layout"
            );
            Ok::<_, anyhow::Error>(())
        }
        .await;
        p4.delete_client(&connection, attempt).await?;
        result?;
    }
    Ok(())
}
