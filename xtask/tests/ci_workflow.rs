//! Contracts for the checked-in ordinary and release workflows.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use yaml_rust2::{Yaml, YamlLoader};

fn workflow(name: &str) -> Result<Yaml> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("workspace parent")?;
    let source = fs::read_to_string(root.join(".github/workflows").join(name))?;
    let mut documents = YamlLoader::load_from_str(&source)?;
    ensure!(documents.len() == 1, "workflow must contain one document");
    let document = documents.remove(0);
    assert!(
        document.as_hash().is_some(),
        "workflow root must be a mapping"
    );
    Ok(document)
}

fn runs(job: &Yaml) -> Vec<&str> {
    job["steps"]
        .as_vec()
        .into_iter()
        .flatten()
        .filter_map(|step| step["run"].as_str())
        .collect()
}

const ROLES: [&str; 13] = [
    "quality",
    "native-windows",
    "native-macos",
    "msrv",
    "no-std",
    "corpus",
    "metrics",
    "work",
    "oracle",
    "coverage",
    "mutation",
    "policy-proofs",
    "fuzz",
];

fn role_commands(role: &str) -> Result<&'static [&'static str]> {
    match role {
        "quality" => Ok(&["-- just check"]),
        "native-windows" | "native-macos" => Ok(&["just test"]),
        "msrv" => Ok(&[
            "cargo +1.88 check -p webpkit -p webpkit-cli --locked",
            "cargo +1.88 check -p webpkit --features rayon,image --locked",
            "cargo +1.88 check -p webpkit --no-default-features --features alloc --locked",
        ]),
        "no-std" => Ok(&["just no-std"]),
        "corpus" => Ok(&["cargo run --locked --release --quiet -p xtask -- corpus-sweep"]),
        "metrics" => Ok(&[
            "cargo +1.96 run --locked --release --quiet -p xtask -- metrics",
            "cargo +1.96 run --locked --release --quiet -p xtask -- metrics --lossy",
        ]),
        "work" => {
            Ok(&["cargo run --locked --release --quiet --features work-count -p xtask -- work"])
        },
        "oracle" => Ok(&[
            "cargo test --locked -p webpkit --features oracle --test oracle --test oracle_lossless --test oracle_lossy --test oracle_rescale",
        ]),
        "coverage" => Ok(&["just coverage"]),
        "mutation" => Ok(&[
            "cargo mutants --shard \"${MUTANT_SHARD}/32\" -p webpkit -p webpkit-cli --test-tool nextest",
        ]),
        "policy-proofs" => Ok(&[
            "\"$POLICY_TOOL\" prove-source --source xtask/src/ci.rs --require-json \"$REQUIRED_HARNESSES\" --counterexample \"$COUNTEREXAMPLE\"",
        ]),
        "fuzz" => Ok(&["just fuzz-smoke"]),
        _ => anyhow::bail!("unknown qualification role"),
    }
}

fn role_source_contract(job: &Yaml) -> Result<()> {
    ensure!(
        job["needs"].as_vec().context("role dependencies")?
            == &vec![Yaml::String("candidate".to_owned())],
        "role requires the candidate resolver"
    );
    ensure!(
        job["if"].as_str() == Some("needs.candidate.outputs.candidate == 'true'"),
        "role must only run for a candidate"
    );
    ensure!(
        job["outputs"]["revision"].as_str() == Some("${{ steps.source_after.outputs.revision }}"),
        "role must expose its post-check identity"
    );
    let steps = job["steps"].as_vec().context("role steps")?;
    ensure!(
        job["continue-on-error"].is_badvalue() || job["continue-on-error"].as_bool() == Some(false),
        "qualification cannot ignore failures"
    );
    ensure!(
        steps.last().context("nonempty role")?["id"].as_str() == Some("source_after"),
        "post-check must follow all work"
    );
    let checks = runs(job)
        .into_iter()
        .filter(|run| run.contains("ci source"))
        .collect::<Vec<_>>();
    ensure!(checks.len() == 2, "role requires both source checks");
    let check_steps = steps
        .iter()
        .enumerate()
        .filter(|(_, step)| {
            step["run"]
                .as_str()
                .is_some_and(|run| run.contains("ci source"))
        })
        .collect::<Vec<_>>();
    for (_, step) in &check_steps {
        ensure!(
            step["env"]["CANDIDATE_SHA"].as_str()
                == Some("${{ needs.candidate.outputs.revision }}"),
            "source checks require the candidate value"
        );
    }
    ensure!(
        checks
            .iter()
            .all(|run| run.contains("--revision \"$CANDIDATE_SHA\"")
                && run.contains("--output \"$GITHUB_OUTPUT\"")),
        "source checks must bind the candidate and output"
    );
    let checkout = steps
        .iter()
        .find(|step| {
            step["uses"]
                .as_str()
                .is_some_and(|uses| uses.starts_with("actions/checkout@"))
        })
        .context("candidate checkout")?;
    ensure!(
        checkout["with"]["ref"].as_str() == Some("${{ needs.candidate.outputs.revision }}"),
        "checkout must select the candidate"
    );
    ensure!(
        checkout["with"]["persist-credentials"].as_bool() == Some(false),
        "qualification checkout must not retain credentials"
    );
    Ok(())
}

fn role_work_contract(role: &str, job: &Yaml) -> Result<()> {
    let steps = job["steps"].as_vec().context("role work steps")?;
    let before = steps
        .iter()
        .position(|step| {
            step["run"]
                .as_str()
                .is_some_and(|run| run.contains("ci source"))
        })
        .context("pre-check")?;
    let after = steps.len().checked_sub(1).context("post-check")?;
    for command in role_commands(role)? {
        let index = steps
            .iter()
            .position(|step| {
                step["run"].as_str().is_some_and(|run| {
                    run.lines().any(|line| {
                        line.trim() == *command || role == "quality" && line.ends_with(command)
                    })
                })
            })
            .context("missing qualification work command")?;
        ensure!(
            index > before && index < after,
            "work must be between source checks"
        );
        ensure!(
            steps[index]["continue-on-error"].is_badvalue()
                || steps[index]["continue-on-error"].as_bool() == Some(false),
            "work failure must remain fatal"
        );
    }
    Ok(())
}

fn release_contract(source: &Yaml) -> Result<()> {
    let jobs = &source["jobs"];
    let qualification = &jobs["qualify"];
    let needs = qualification["needs"]
        .as_vec()
        .context("qualification dependencies")?;
    ensure!(
        needs.len() == ROLES.len() + 1,
        "qualification inventory must be exact"
    );
    ensure!(
        needs.iter().any(|need| need.as_str() == Some("candidate")),
        "candidate dependency is mandatory"
    );
    for role in ROLES {
        ensure!(
            needs.iter().any(|need| need.as_str() == Some(role)),
            "missing role {role}"
        );
        role_source_contract(&jobs[role])?;
        role_work_contract(role, &jobs[role])?;
    }
    ensure!(
        runs(qualification)
            .iter()
            .any(|run| run.contains("ci qualify")
                && run.contains("--needs \"$NEEDS_JSON\"")
                && run.contains("--output \"$GITHUB_OUTPUT\"")),
        "qualification must execute the complete controller"
    );
    ensure!(
        qualification["outputs"]["revision"].as_str().is_some(),
        "qualification must expose its checked identity"
    );
    let publisher = &jobs["release-plz-release"];
    let publisher_needs = publisher["needs"]
        .as_vec()
        .context("publisher dependencies")?;
    ensure!(
        publisher_needs.len() == 2
            && publisher_needs
                .iter()
                .any(|need| need.as_str() == Some("qualify"))
            && publisher_needs
                .iter()
                .any(|need| need.as_str() == Some("candidate")),
        "publisher requires qualification and candidate"
    );
    let condition = publisher["if"]
        .as_str()
        .context("publisher admission condition")?;
    ensure!(
        condition.contains("needs.qualify.result == 'success'")
            && condition
                .contains("needs.qualify.outputs.revision == needs.candidate.outputs.revision"),
        "publisher must bind successful qualification identity"
    );
    let steps = publisher["steps"].as_vec().context("publisher steps")?;
    let action = steps
        .iter()
        .find(|step| {
            step["uses"].as_str()
                == Some("release-plz/action@8b5bf233ecb5779f63508db7cd050e948ee77cc2")
        })
        .context("immutable publisher action")?;
    ensure!(
        action["with"]["version"].as_str() == Some("0.3.174"),
        "publisher executable version must match the source-selection contract"
    );
    ensure!(
        action["with"]["command"].as_str() == Some("release"),
        "publisher must not run proposal logic"
    );
    ensure!(
        action["with"]["config"].as_str()
            == Some("${{ steps.publish-source.outputs.config_path }}"),
        "publisher must use only the derived configuration"
    );
    ensure!(
        runs(publisher)
            .iter()
            .any(|run| run.contains("ci publish-source")
                && run.contains("--config \"$RELEASE_CONFIG\"")),
        "publisher must revalidate eligibility and derive its config"
    );
    Ok(())
}

#[test]
fn ordinary_summary_uses_the_checked_controller() {
    let source = workflow("ci.yml").unwrap();
    ordinary_contract(&source).unwrap();
    let job = &source["jobs"]["ci-required"];
    assert!(
        runs(job).iter().any(|run| run.contains("ci required")),
        "ordinary summary must invoke the production strict controller"
    );
    let needs = job["needs"].as_vec().unwrap();
    assert_eq!(needs.len(), 2);
    assert!(needs.iter().any(|need| need.as_str() == Some("quality")));
    assert!(
        needs
            .iter()
            .any(|need| need.as_str() == Some("conventional-commits"))
    );
}

fn ordinary_contract(source: &Yaml) -> Result<()> {
    ensure!(
        source["jobs"].as_hash().context("ordinary jobs")?.len() == 4,
        "ordinary work inventory must remain lean"
    );
    let jobs = &source["jobs"];
    ensure!(
        runs(&jobs["quality"])
            .iter()
            .any(|run| run.ends_with("-- just check")),
        "ordinary quality must execute the complete project gate"
    );
    let required = &jobs["ci-required"];
    ensure!(
        required["if"].as_str() == Some("always()"),
        "ordinary aggregate must observe unsuccessful work"
    );
    let needs = required["needs"]
        .as_vec()
        .context("ordinary dependencies")?;
    ensure!(
        needs.len() == 2
            && needs.iter().any(|need| need.as_str() == Some("quality"))
            && needs
                .iter()
                .any(|need| need.as_str() == Some("conventional-commits")),
        "ordinary aggregate inventory must be exact"
    );
    ensure!(
        runs(required)
            .iter()
            .any(|run| run.contains("ci required --event \"$EVENT_NAME\" --needs \"$NEEDS_JSON\"")),
        "ordinary aggregate must use the controller"
    );
    let final_job = &jobs["required"];
    ensure!(
        final_job["if"].as_str() == Some("always()"),
        "required context must observe failures"
    );
    ensure!(
        final_job["needs"].as_vec().context("final dependencies")?
            == &vec![Yaml::String("ci-required".to_owned())],
        "required context must depend on the strict aggregate"
    );
    ensure!(
        runs(final_job)
            .iter()
            .any(|run| run.contains("ci required --final --needs \"$NEEDS_JSON\"")),
        "required context must use strict final admission"
    );
    Ok(())
}

fn just_check_contract(source: &str) -> Result<()> {
    let (_, tail) = source
        .split_once("\ncheck:\n")
        .context("ordinary check recipe")?;
    let commands = tail
        .lines()
        .take_while(|line| line.is_empty() || line.starts_with(' '))
        .map(str::trim)
        .collect::<Vec<_>>();
    for command in ["just lint", "just test", "just doc", "just no-std-host"] {
        ensure!(
            commands.contains(&command),
            "ordinary check lacks {command}"
        );
    }
    ensure!(
        !commands.contains(&"just image"),
        "ordinary check must not repeat the workspace image graph"
    );
    Ok(())
}

const WORKSPACE_FEATURES: &str = "--features webpkit/image,webpkit/rayon";
const WORKSPACE_COMMANDS: [&str; 5] = [
    "cargo clippy --workspace",
    "cargo nextest run --workspace",
    "cargo test --workspace",
    "cargo test --doc --workspace",
    "RUSTDOCFLAGS=\"-D warnings\" cargo doc --workspace",
];

fn workspace_feature_contract(source: &str) -> Result<()> {
    for prefix in WORKSPACE_COMMANDS {
        let mut commands = source
            .lines()
            .filter(|line| line.trim_start().starts_with(prefix));
        let command = commands.next().context("missing workspace entry point")?;
        ensure!(
            commands.next().is_none(),
            "duplicate workspace entry point {prefix}"
        );
        ensure!(
            command.contains(WORKSPACE_FEATURES),
            "workspace entry point lacks explicit public features: {prefix}"
        );
    }
    just_check_contract(source)
}

#[test]
fn ordinary_workspace_commands_pin_public_features_without_duplicate_image() {
    let source = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("Justfile"),
    )
    .unwrap()
    .replace("\r\n", "\n");
    workspace_feature_contract(&source).unwrap();
    for prefix in WORKSPACE_COMMANDS {
        let command = source
            .lines()
            .find(|line| line.trim_start().starts_with(prefix))
            .unwrap();
        let missing_features =
            source.replacen(command, &command.replace(WORKSPACE_FEATURES, ""), 1);
        assert!(
            workspace_feature_contract(&missing_features).is_err(),
            "missing public features for {prefix}"
        );
    }
    let duplicate_image = source.replacen("\ncheck:\n", "\ncheck:\n    just image\n", 1);
    assert!(workspace_feature_contract(&duplicate_image).is_err());
}

#[test]
fn deleting_ordinary_tests_or_weakening_required_is_rejected() {
    let original = workflow("ci.yml").unwrap();
    ordinary_contract(&original).unwrap();
    let mut changed = original.clone();
    changed["jobs"]["required"]["if"] = Yaml::String("success()".to_owned());
    assert!(ordinary_contract(&changed).is_err());
    let mut changed = original;
    let Yaml::Array(steps) = &mut changed["jobs"]["quality"]["steps"] else {
        unreachable!()
    };
    steps.retain(|step| {
        !step["run"]
            .as_str()
            .is_some_and(|run| run.ends_with("-- just check"))
    });
    assert!(ordinary_contract(&changed).is_err());
    let source = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("Justfile"),
    )
    .unwrap()
    .replace("\r\n", "\n");
    just_check_contract(&source).unwrap();
    assert!(just_check_contract(&source.replacen("    just test\n", "", 1)).is_err());
}

#[test]
fn publication_requires_complete_same_source_qualification() {
    let source = workflow("release-plz.yml").unwrap();
    release_contract(&source).unwrap();
    let jobs = &source["jobs"];
    let qualification = &jobs["qualify"];
    assert!(
        runs(qualification)
            .iter()
            .any(|run| run.contains("ci qualify")),
        "release qualification must invoke the production strict controller"
    );
    let roles = [
        "quality",
        "native-windows",
        "native-macos",
        "msrv",
        "no-std",
        "corpus",
        "metrics",
        "work",
        "oracle",
        "coverage",
        "mutation",
        "policy-proofs",
        "fuzz",
    ];
    let needs = qualification["needs"].as_vec().unwrap();
    for role in roles {
        assert!(
            needs.iter().any(|need| need.as_str() == Some(role)),
            "missing role {role}"
        );
    }
    let publisher = &jobs["release-plz-release"];
    assert!(
        publisher["needs"]
            .as_vec()
            .unwrap()
            .iter()
            .any(|need| need.as_str() == Some("qualify"))
    );
    assert!(
        runs(publisher)
            .iter()
            .any(|run| run.contains("ci publish-source"))
    );
}

#[test]
fn removing_a_role_source_guard_or_publisher_binding_is_rejected() {
    let original = workflow("release-plz.yml").unwrap();
    release_contract(&original).unwrap();
    for role in ROLES {
        let mut changed = original.clone();
        let Yaml::Hash(jobs) = &mut changed["jobs"] else {
            unreachable!()
        };
        jobs.remove(&Yaml::String(role.to_owned()));
        assert!(release_contract(&changed).is_err(), "deleted {role}");
        let mut changed = original.clone();
        changed["jobs"][role]["outputs"]["revision"] =
            Yaml::String("${{ steps.source_before.outputs.revision }}".to_owned());
        assert!(
            release_contract(&changed).is_err(),
            "pre-check identity for {role}"
        );
        let mut changed = original.clone();
        let Yaml::Array(steps) = &mut changed["jobs"][role]["steps"] else {
            unreachable!()
        };
        let commands = role_commands(role).unwrap();
        steps.retain(|step| {
            !step["run"]
                .as_str()
                .is_some_and(|run| commands.iter().any(|command| run.contains(command)))
        });
        assert!(
            release_contract(&changed).is_err(),
            "deleted work for {role}"
        );
    }
    let mut changed = original.clone();
    changed["jobs"]["release-plz-release"]["if"] = Yaml::String("always()".to_owned());
    assert!(release_contract(&changed).is_err());
    let mut changed = original;
    let Yaml::Array(steps) = &mut changed["jobs"]["release-plz-release"]["steps"] else {
        unreachable!()
    };
    let action = steps
        .iter_mut()
        .find(|step| {
            step["uses"]
                .as_str()
                .is_some_and(|uses| uses.starts_with("release-plz/action@"))
        })
        .unwrap();
    action["with"]["config"] = Yaml::String("release-plz.toml".to_owned());
    assert!(release_contract(&changed).is_err());
}

#[test]
fn proof_role_binds_the_exact_canonical_source_and_complete_inventory() {
    let source = workflow("release-plz.yml").unwrap();
    let steps = source["jobs"]["policy-proofs"]["steps"].as_vec().unwrap();
    assert!(steps.iter().any(|step| step["uses"].as_str() == Some("P4suta/project-template/.github/actions/ci-policy@ee6265bd944c53097191f97dcb7a6ac678794b43")));
    let invocation = steps
        .iter()
        .find(|step| {
            step["run"]
                .as_str()
                .is_some_and(|run| run.contains("prove-source"))
        })
        .unwrap();
    let expected = [
        "production::proofs::commit_identity_is_exact",
        "production::proofs::candidate_selection_preserves_source",
        "production::proofs::ordinary_summary_requires_complete_work",
        "production::proofs::publication_requires_complete_same_source_qualification",
    ];
    let names: Vec<String> =
        serde_json::from_str(invocation["env"]["REQUIRED_HARNESSES"].as_str().unwrap()).unwrap();
    assert_eq!(names, expected);
    assert_eq!(
        invocation["env"]["COUNTEREXAMPLE"].as_str(),
        Some("production::proofs::reject_incomplete_qualification_probe")
    );
    assert!(
        invocation["run"]
            .as_str()
            .unwrap()
            .contains("--source xtask/src/ci.rs --require-json")
    );
    let canonical =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ci.rs")).unwrap();
    assert_eq!(
        canonical.matches("#[kani::proof]").count(),
        expected.len() + 1
    );
    for name in expected {
        assert!(canonical.contains(name.rsplit("::").next().unwrap()));
    }
}

#[test]
fn yaml_boundary_rejects_duplicate_keys_and_exposes_all_documents() {
    assert!(YamlLoader::load_from_str("jobs: {}\njobs: {}\n").is_err());
    assert_eq!(
        YamlLoader::load_from_str("---\njobs: {}\n---\njobs: {}\n")
            .unwrap()
            .len(),
        2
    );
}
