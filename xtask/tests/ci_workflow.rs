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
    let quality = &jobs["quality"];
    let steps = quality["steps"]
        .as_vec()
        .context("ordinary quality steps")?;
    let dispatch = steps
        .iter()
        .find(|step| {
            step["run"]
                .as_str()
                .is_some_and(|run| run.ends_with("-- just check-changes"))
        })
        .context("ordinary checked dispatch")?;
    ensure!(
        dispatch["env"]["CI_BASE_REVISION"].as_str()
            == Some(
                "${{ github.event.pull_request.base.sha || github.event.merge_group.base_sha }}"
            )
            && dispatch["env"]["CI_HEAD_REVISION"].as_str() == Some("${{ github.sha }}"),
        "ordinary dispatch must compare the checked event source"
    );
    ensure!(
        dispatch["if"].is_badvalue()
            && dispatch["continue-on-error"].is_badvalue()
            && quality["if"].is_badvalue()
            && quality["continue-on-error"].is_badvalue(),
        "ordinary dispatch cannot skip work or ignore its failure"
    );
    let checkout = steps
        .iter()
        .find(|step| {
            step["uses"]
                .as_str()
                .is_some_and(|uses| uses.starts_with("actions/checkout@"))
        })
        .context("ordinary source checkout")?;
    ensure!(
        checkout["with"]["fetch-depth"].as_i64() == Some(0)
            && checkout["with"]["persist-credentials"].as_bool() == Some(false),
        "ordinary selection needs comparison history without retained credentials"
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

fn codeql_contract(source: &Yaml) -> Result<()> {
    let jobs = &source["jobs"];
    ensure!(
        jobs.as_hash().context("CodeQL jobs")?.len() == 2,
        "CodeQL requires one planner and one analysis matrix"
    );
    let plan = &jobs["plan"];
    ensure!(
        plan["outputs"]["matrix"].as_str() == Some("${{ steps.selection.outputs.matrix }}")
            && plan["if"].is_badvalue()
            && plan["continue-on-error"].is_badvalue(),
        "CodeQL must expose an unconditional checked selection"
    );
    let steps = plan["steps"].as_vec().context("CodeQL planner steps")?;
    let selection = steps
        .iter()
        .find(|step| step["id"].as_str() == Some("selection"))
        .context("CodeQL selection producer")?;
    ensure!(
        selection["env"]["CI_BASE_REVISION"].as_str()
            == Some("${{ github.event.pull_request.base.sha }}")
            && selection["env"]["CI_HEAD_REVISION"].as_str() == Some("${{ github.sha }}"),
        "CodeQL must bind PR changes and select both languages for the main baseline"
    );
    let commands = selection["run"]
        .as_str()
        .context("CodeQL planner command")?;
    let commands = commands.lines().map(str::trim).collect::<Vec<_>>();
    ensure!(
        commands
            == [
                "matrix=$(cargo run --locked --quiet -p webpkit-ci-select --bin ci-select -- codeql-matrix)",
                "printf 'matrix=%s\\n' \"$matrix\" >> \"$GITHUB_OUTPUT\"",
            ]
            && selection["shell"].as_str() == Some("bash")
            && selection["if"].is_badvalue()
            && selection["continue-on-error"].is_badvalue(),
        "CodeQL must propagate selector failures before publishing its matrix"
    );
    let checkout = steps
        .iter()
        .find(|step| {
            step["uses"]
                .as_str()
                .is_some_and(|uses| uses.starts_with("actions/checkout@"))
        })
        .context("CodeQL planner checkout")?;
    ensure!(
        checkout["with"]["fetch-depth"].as_i64() == Some(0)
            && checkout["with"]["persist-credentials"].as_bool() == Some(false),
        "CodeQL selection needs comparison history without retained credentials"
    );
    let analyze = &jobs["analyze"];
    ensure!(
        analyze["needs"].as_vec() == Some(&vec![Yaml::String("plan".to_owned())])
            && analyze["strategy"]["matrix"].as_str()
                == Some("${{ fromJSON(needs.plan.outputs.matrix) }}")
            && analyze["if"].is_badvalue()
            && analyze["continue-on-error"].is_badvalue(),
        "CodeQL analysis must consume the successful planner matrix"
    );
    let steps = analyze["steps"].as_vec().context("CodeQL analysis steps")?;
    let init = steps
        .iter()
        .find(|step| {
            step["uses"]
                .as_str()
                .is_some_and(|uses| uses.starts_with("github/codeql-action/init@"))
        })
        .context("CodeQL initialization")?;
    ensure!(
        init["with"]["languages"].as_str() == Some("${{ matrix.language }}")
            && init["with"]["build-mode"].as_str() == Some("${{ matrix.build-mode }}")
            && init["with"]["queries"].as_str() == Some("security-extended")
            && init["if"].is_badvalue()
            && init["continue-on-error"].is_badvalue(),
        "CodeQL must analyze the selected language with the security suite"
    );
    let upload = steps
        .iter()
        .find(|step| {
            step["uses"]
                .as_str()
                .is_some_and(|uses| uses.starts_with("github/codeql-action/analyze@"))
        })
        .context("CodeQL result upload")?;
    ensure!(
        upload["with"]["category"].as_str() == Some("/language:${{ matrix.language }}")
            && upload["if"].is_badvalue()
            && upload["continue-on-error"].is_badvalue(),
        "CodeQL must publish each successful language analysis"
    );
    ensure!(
        source["on"]["schedule"].is_badvalue()
            && source["on"]["push"]["branches"].as_vec()
                == Some(&vec![Yaml::String("main".to_owned())])
            && !source["on"]["pull_request"].is_badvalue(),
        "CodeQL must check changes and publish the main baseline without a schedule"
    );
    Ok(())
}

#[test]
fn codeql_uses_the_checked_source_selection() {
    codeql_contract(&workflow("codeql.yml").unwrap()).unwrap();
}

#[test]
fn disconnected_or_failure_masking_codeql_selection_is_rejected() {
    let original = workflow("codeql.yml").unwrap();
    codeql_contract(&original).unwrap();
    let mut changed = original.clone();
    changed["jobs"]["analyze"]["strategy"]["matrix"] = Yaml::String("{}".to_owned());
    assert!(codeql_contract(&changed).is_err());
    let mut changed = original.clone();
    let Yaml::Array(steps) = &mut changed["jobs"]["plan"]["steps"] else {
        unreachable!()
    };
    let selection = steps
        .iter_mut()
        .find(|step| step["id"].as_str() == Some("selection"))
        .unwrap();
    selection["run"] = Yaml::String("printf 'matrix={}\\n' >> \"$GITHUB_OUTPUT\"".to_owned());
    assert!(codeql_contract(&changed).is_err());
    let mut changed = original;
    let Yaml::Array(steps) = &mut changed["jobs"]["plan"]["steps"] else {
        unreachable!()
    };
    let selection = steps
        .iter_mut()
        .find(|step| step["id"].as_str() == Some("selection"))
        .unwrap();
    selection["env"]["CI_HEAD_REVISION"] = Yaml::String("main".to_owned());
    assert!(codeql_contract(&changed).is_err());
    let mut changed = workflow("codeql.yml").unwrap();
    let Yaml::Array(steps) = &mut changed["jobs"]["plan"]["steps"] else {
        unreachable!()
    };
    let selection = steps
        .iter_mut()
        .find(|step| step["id"].as_str() == Some("selection"))
        .unwrap();
    selection["env"]["CI_BASE_REVISION"] =
        Yaml::String("${{ github.event.pull_request.base.sha || github.event.before }}".to_owned());
    assert!(codeql_contract(&changed).is_err());
    for job in ["plan", "analyze"] {
        let mut changed = workflow("codeql.yml").unwrap();
        let Yaml::Hash(job) = &mut changed["jobs"][job] else {
            unreachable!()
        };
        job.insert(
            Yaml::String("continue-on-error".to_owned()),
            Yaml::Boolean(true),
        );
        assert!(codeql_contract(&changed).is_err());
    }
}

fn changed_gate_contract(source: &str, hooks: &Yaml) -> Result<()> {
    ensure!(
        source.contains("\nactionlint:\n    actionlint\n"),
        "workflow lint must discover both supported YAML extensions"
    );
    let (_, tail) = source
        .split_once("\ncheck-workflows:\n")
        .context("workflow check recipe")?;
    let commands = tail
        .lines()
        .take_while(|line| line.is_empty() || line.starts_with(' '))
        .map(str::trim)
        .collect::<Vec<_>>();
    for command in [
        "just actionlint",
        "yamlfmt --lint .github/workflows",
        "just typos",
        "cargo test --locked -p xtask --test ci_workflow",
    ] {
        ensure!(commands.contains(&command), "workflow gate lacks {command}");
    }
    let (_, tail) = source
        .split_once("\ncheck-changes:\n")
        .context("checked selection recipe")?;
    ensure!(
        tail.lines().next()
            == Some("    cargo run --locked --quiet -p webpkit-ci-select --bin ci-select -- check"),
        "local selection must run the production adapter"
    );
    ensure!(
        hooks["pre-push"]["commands"]["check"]["run"].as_str() == Some("just check-changes"),
        "pre-push must share the hosted selection entry point"
    );
    Ok(())
}

#[test]
fn local_selection_retains_workflow_contracts_and_the_full_gate() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let source = fs::read_to_string(root.join("Justfile"))
        .unwrap()
        .replace("\r\n", "\n");
    let hooks = fs::read_to_string(root.join("lefthook.yml")).unwrap();
    let hooks = YamlLoader::load_from_str(&hooks).unwrap().remove(0);
    just_check_contract(&source).unwrap();
    changed_gate_contract(&source, &hooks).unwrap();
    let missing_contracts = source.replacen(
        "    cargo test --locked -p xtask --test ci_workflow\n",
        "",
        1,
    );
    assert!(changed_gate_contract(&missing_contracts, &hooks).is_err());
    let disconnected = source.replacen("--bin ci-select -- check", "--bin ci-select -- help", 1);
    assert!(changed_gate_contract(&disconnected, &hooks).is_err());
    let missing_yaml = source.replacen(
        "\nactionlint:\n    actionlint\n",
        "\nactionlint:\n    actionlint .github/workflows/*.yml\n",
        1,
    );
    assert!(changed_gate_contract(&missing_yaml, &hooks).is_err());
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
            .is_some_and(|run| run.ends_with("-- just check-changes"))
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
        "production::proofs::affected_work_routes_recognized_workflow_boundaries",
        "production::proofs::affected_work_routes_recognized_rust_and_action_inputs",
        "production::proofs::affected_work_unknown_and_empty_inputs_keep_full_selection",
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
