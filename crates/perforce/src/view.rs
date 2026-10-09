use anyhow::{Context, Result, ensure};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Mapping {
    depot: String,
    local: String,
    excluded: bool,
}

fn words(line: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    let mut chars = line.chars().peekable();
    while chars.peek().is_some() {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let quoted = chars.peek() == Some(&'"');
        if quoted {
            chars.next();
        }
        let mut word = String::new();
        let mut closed = !quoted;
        for c in chars.by_ref() {
            if quoted && c == '"' {
                closed = true;
                break;
            }
            if !quoted && c.is_whitespace() {
                break;
            }
            ensure!(c != '"', "quotes must surround a complete path");
            word.push(c);
        }
        ensure!(closed && !word.is_empty(), "invalid quoted View path");
        if quoted {
            ensure!(
                chars.peek().is_none_or(|c| c.is_whitespace()),
                "separate View paths with whitespace"
            );
        }
        result.push(word);
    }
    Ok(result)
}

fn prefix(path: &str, depot: bool) -> Result<String> {
    ensure!(
        path.len() <= 512
            && !path
                .chars()
                .any(|c| c.is_control() || "\\@#%*\"?:<>|".contains(c)),
        "unsupported character in View path"
    );
    let relative = if depot {
        path.strip_prefix("//")
            .context("View source must start with //depot/")?
    } else {
        path
    };
    let base = if relative == "..." && !depot {
        ""
    } else {
        relative
            .strip_suffix("/...")
            .context("View paths must end with /... (or ... for the workspace root)")?
    };
    ensure!(
        base.is_empty() && !depot
            || (!base.is_empty()
                && base
                    .split('/')
                    .all(|c| !c.is_empty() && c != "." && c != ".." && !c.contains("..."))),
        "View path must remain inside its depot/workspace"
    );
    Ok(if depot {
        format!("//{base}/")
    } else if base.is_empty() {
        String::new()
    } else {
        format!("{base}/")
    })
}

fn mappings(lines: &[String]) -> Result<Vec<Mapping>> {
    ensure!(
        !lines.is_empty() && lines.len() <= 64,
        "View needs 1..64 lines"
    );
    let mut result: Vec<Mapping> = Vec::new();
    let mut includes: Vec<Mapping> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let parse = || -> Result<(String, Option<String>, bool)> {
            ensure!(
                !line.chars().any(|c| c.is_control() && c != '\t'),
                "each View entry must be a single line"
            );
            let line = line.trim();
            let (excluded, rest) = line.strip_prefix('-').map_or((false, line), |s| (true, s));
            ensure!(
                !rest.starts_with(char::is_whitespace),
                "no space is allowed after -"
            );
            let parts = words(rest)?;
            ensure!(
                parts.len() == 2 || excluded && parts.len() == 1,
                "use //depot/path/... relative/... or -//depot/path/..."
            );
            Ok((
                prefix(&parts[0], true)?,
                parts.get(1).map(|p| prefix(p, false)).transpose()?,
                excluded,
            ))
        };
        let (depot, local, excluded) =
            parse().map_err(|error| anyhow::anyhow!("View line {}: {error}", index + 1))?;
        if let Some(local) = local {
            let mapping = Mapping {
                depot,
                local,
                excluded,
            };
            if !excluded {
                includes.push(mapping.clone());
            }
            result.push(mapping);
        } else {
            // A one-column exclusion applies to every earlier included mapping.
            // Disjoint exclusions are harmless; a later inclusion can re-include.
            for include in &includes {
                if let Some(suffix) = depot.strip_prefix(&include.depot) {
                    result.push(Mapping {
                        depot: depot.clone(),
                        local: format!("{}{suffix}", include.local),
                        excluded: true,
                    });
                } else if include.depot.starts_with(&depot) {
                    result.push(Mapping {
                        excluded: true,
                        ..include.clone()
                    });
                }
            }
        }
        ensure!(
            result.len() <= 1024,
            "View expands to too many client mappings"
        );
    }
    ensure!(
        !includes.is_empty(),
        "View needs at least one included mapping"
    );
    Ok(result)
}

/// Resolve user-relative View rules to one generated client name, in input order.
pub(crate) fn client_view(lines: &[String], client: &str) -> Result<Vec<String>> {
    ensure!(
        !client.is_empty()
            && client
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_'),
        "invalid generated client name"
    );
    let quote = |path: String| {
        if path.chars().any(char::is_whitespace) {
            format!("\"{path}\"")
        } else {
            path
        }
    };
    Ok(mappings(lines)?
        .into_iter()
        .map(|mapping| {
            format!(
                "{} {}",
                quote(format!(
                    "{}{}...",
                    if mapping.excluded { "-" } else { "" },
                    mapping.depot
                )),
                quote(format!("//{client}/{}...", mapping.local))
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn resolve(lines: &[&str]) -> Result<Vec<String>> {
        client_view(
            &lines.iter().map(|s| (*s).into()).collect::<Vec<_>>(),
            "test_client",
        )
    }
    #[test]
    fn server_maps_to_workspace_root_and_unrelated_client_is_excluded() {
        assert_eq!(
            resolve(&["//stream/root/Server/... ...", "-//stream/root/Client/..."]).unwrap(),
            ["//stream/root/Server/... //test_client/..."]
        );
    }
    #[test]
    fn exclusions_follow_remaps_and_later_includes_keep_their_order() {
        assert_eq!(
            resolve(&[
                "//depot/root/... src/...",
                "-//depot/root/Client/...",
                "//depot/root/Client/Keep/... keep/...",
                "-//depot/root/Client/Keep/cache/..."
            ])
            .unwrap(),
            [
                "//depot/root/... //test_client/src/...",
                "-//depot/root/Client/... //test_client/src/Client/...",
                "//depot/root/Client/Keep/... //test_client/keep/...",
                "-//depot/root/Client/Keep/cache/... //test_client/src/Client/Keep/cache/...",
                "-//depot/root/Client/Keep/cache/... //test_client/keep/cache/...",
            ]
        );
        assert_eq!(
            resolve(&["//depot/root/Server/... ...", "-//depot/root/..."]).unwrap()[1],
            "-//depot/root/Server/... //test_client/..."
        );
    }
    #[test]
    fn supports_quoted_paths_and_explicit_exclusion_destinations() {
        assert_eq!(
            resolve(&[
                "\"//depot/한글 folder/...\" \"local folder/...\"",
                "-//depot/Client/... Client/..."
            ])
            .unwrap(),
            [
                "\"//depot/한글 folder/...\" \"//test_client/local folder/...\"",
                "-//depot/Client/... //test_client/Client/..."
            ]
        );
    }
    #[test]
    fn rejects_escape_injection_and_unsupported_view_syntax() {
        for bad in [
            "//depot/root/... ../...",
            "//depot/root/... /tmp/...",
            "//depot/root/... //other/...",
            "//depot/root/... C:/...",
            "//depot/../... ...",
            "//depot/root/...@12 ...",
            "//depot/root/... ...\nOwner: other",
            "+//depot/root/... ...",
            "//depot/root/* ...",
            "//depot/root/...",
            "- //depot/root/...",
            "-//depot/root/...",
            "\"//depot/root/... ...",
        ] {
            assert!(resolve(&[bad]).is_err(), "{bad}");
        }
    }
}
