//! `columbus-mcp env …`: the person registers, confirms and removes their
//! Python environments at the keyboard (build brief Part 5, F-4). Every
//! change is confirmed by typing the environment's id on the terminal
//! itself (not stdin), as `servers approve` is, so an agent that can run
//! commands cannot register, change or confirm one. No MCP tool does any
//! of this.

use std::path::PathBuf;

use anyhow::{anyhow, bail, Result};
use columbus_shared::environments::{self as envs, Confinement, CreatedBy, Registration, Trust};

use crate::config::{self, Paths};

fn now() -> String {
    columbus_shared::tools::now()
}

fn show_probe(found: &envs::Found) {
    println!("  Python {}", found.python_version);
    println!(
        "  {} packages, packages hash {}",
        found.packages.len(),
        found.packages_sha256()
    );
    if let Some(cm) = &found.conda_meta {
        println!("  conda-meta: {} files", cm.len());
    }
    if found.arcpy {
        println!("  arcpy: yes {}", envs::EXPERIMENTAL);
        println!(
            "  ArcGIS Pro licence: {}",
            envs::licence_words(found.licence.as_deref())
        );
    } else {
        println!("  arcpy: no");
    }
}

fn probe(python: &std::path::Path) -> Result<envs::Found> {
    println!(
        "Probing {} (at most {} s)...",
        python.display(),
        envs::probe_limit().as_secs()
    );
    envs::probe(python, envs::probe_limit()).map_err(|e| anyhow!(e))
}

/// `env add <id> --python <path> [--trust governed|trusted]
/// [--confinement A|B] [--description TEXT]`.
pub fn add(id: &str, rest: &[&str]) -> Result<()> {
    let mut python = None;
    let mut trust = Trust::Governed;
    let mut confinement = Confinement::A;
    let mut description = None;
    let mut i = 0;
    while i < rest.len() {
        let value = rest
            .get(i + 1)
            .ok_or_else(|| anyhow!("{} needs a value", rest[i]))?;
        match rest[i] {
            "--python" => python = Some(PathBuf::from(value)),
            "--trust" => {
                trust = Trust::parse(value)
                    .ok_or_else(|| anyhow!("--trust must be governed or trusted"))?
            }
            "--confinement" => {
                confinement = Confinement::parse(value)
                    .ok_or_else(|| anyhow!("--confinement must be A or B"))?
            }
            "--description" => description = Some(value.to_string()),
            other => bail!("unknown argument {other:?}"),
        }
        i += 2;
    }
    let python = python.ok_or_else(|| anyhow!("--python <path to the interpreter> is required"))?;
    let paths = Paths::from_env();
    let (policy, _) = config::load_policy(&paths)?;
    let p = &policy.environments;
    // Refused before anything runs.
    if !p.allow {
        bail!("organization policy does not allow environments (environments.allow is off), so none can be registered");
    }
    if !envs::id_ok(id) {
        bail!("{id:?} is not an environment id (lowercase letters, digits and -, at most 40)");
    }
    if trust == Trust::Trusted && !p.allow_trusted {
        bail!("organization policy does not allow trusted environments (environments.allow_trusted is off); register it as governed");
    }
    if confinement == Confinement::B && !p.allow_network {
        bail!("organization policy does not allow governed environments network access (environments.allow_network is off); use confinement A");
    }
    if paths.environments.join(format!("{id}.yaml")).exists() {
        bail!("an environment {id:?} is already registered; remove it first, or confirm it again with `columbus-mcp env confirm {id}`");
    }
    envs::check_interpreter(&python).map_err(|e| anyhow!(e))?;
    let found = probe(&python)?;
    println!(
        "\nEnvironment {id}: {} ({})",
        python.display(),
        trust.as_str()
    );
    show_probe(&found);
    if trust == Trust::Governed {
        println!(
            "  Scripts in it run confined at level {}: {}, extracts only.",
            confinement.as_str(),
            if confinement == Confinement::B {
                "they can reach the internet"
            } else {
                "no network"
            }
        );
    } else {
        println!(
            "  Trusted {}: scripts in it run unconfined, as you, and every run is approved. {}",
            envs::EXPERIMENTAL,
            envs::TRUSTED_CARD
        );
    }
    let typed = crate::review_cli::confirm_typed(&format!(
        "\nRegister {id} so your assistant's scripts can run in it? Type the environment id to confirm: "
    ))?;
    let reg = Registration {
        id: id.to_string(),
        python,
        trust,
        confinement,
        created_by: CreatedBy::Person,
        description,
    };
    let env = envs::register(
        &paths.environments,
        &paths.envs,
        p,
        &reg,
        &found,
        &typed,
        &now(),
    )
    .map_err(|e| anyhow!(e))?;
    println!("Registered {} in {}.", env.id, paths.environments.display());
    Ok(())
}

/// `env confirm <id>`: probe again and confirm the registration as its
/// file stands now (after a change marked it `changed`).
pub fn confirm(id: &str) -> Result<()> {
    let paths = Paths::from_env();
    let (policy, _) = config::load_policy(&paths)?;
    if !policy.environments.allow {
        bail!("organization policy does not allow environments (environments.allow is off)");
    }
    let env = envs::pending(&paths.environments, id).map_err(|e| anyhow!(e))?;
    let v = envs::view(&paths.environments, id);
    if let Some(r) = v.status.reason() {
        println!("{id} is {}: {r}.", v.status.as_str());
    }
    let python = PathBuf::from(&env.python);
    envs::check_interpreter(&python).map_err(|e| anyhow!(e))?;
    let found = probe(&python)?;
    println!(
        "\nEnvironment {id}: {} ({}, confinement {}, created by {})",
        env.python,
        env.trust.as_str(),
        env.confinement.as_str(),
        env.created_by.as_str()
    );
    show_probe(&found);
    let typed = crate::review_cli::confirm_typed(&format!(
        "\nConfirm {id} as shown? Type the environment id to confirm: "
    ))?;
    envs::confirm(
        &paths.environments,
        &paths.envs,
        &policy.environments,
        id,
        &found,
        &typed,
        &now(),
    )
    .map_err(|e| anyhow!(e))?;
    println!("Confirmed {id}.");
    Ok(())
}

/// `env list`: every registration and its state.
pub fn list() -> Result<()> {
    let paths = Paths::from_env();
    let (policy, _) = config::load_policy(&paths)?;
    println!("Environments in {}", paths.environments.display());
    if !policy.environments.allow {
        println!("note: organization policy turns environments off (environments.allow); none of these are used.");
    }
    for v in envs::list(&paths.environments) {
        match &v.env {
            Some(e) => println!(
                "{:<20} {:<9} {:<8} Python {:<9} {}  {}",
                v.id,
                e.trust.as_str(),
                v.status.as_str(),
                e.probe.python_version,
                e.arcpy_line(),
                v.status.reason().unwrap_or("")
            ),
            None => println!("{:<20} invalid   {}", v.id, v.status.reason().unwrap_or("")),
        }
    }
    Ok(())
}

/// `env remove <id>`: delete its registration; an environment Columbus
/// created also loses its folder under `<Columbus data>/envs/`. A folder
/// the person registered is never deleted.
pub fn remove(id: &str) -> Result<()> {
    let paths = Paths::from_env();
    let plan = envs::removal(&paths.environments, &paths.envs, id).map_err(|e| anyhow!(e))?;
    println!("Removing {id} deletes {}.", plan.yaml.display());
    match &plan.folder {
        Some(f) => println!(
            "Columbus created it, so its folder {} is deleted too.",
            f.display()
        ),
        None => println!("Its interpreter folder is left as it is."),
    }
    let typed = crate::review_cli::confirm_typed(&format!(
        "\nRemove {id}? Type the environment id to confirm: "
    ))?;
    let done =
        envs::remove(&paths.environments, &paths.envs, id, &typed).map_err(|e| anyhow!(e))?;
    println!(
        "Removed {id}{}.",
        if done.folder.is_some() {
            " and its folder"
        } else {
            ""
        }
    );
    Ok(())
}

/// `env proposals`: environments the assistant asked for, waiting.
pub fn proposals() -> Result<()> {
    let paths = Paths::from_env();
    let waiting = columbus_shared::env_create::list_proposals(&paths.environments);
    if waiting.is_empty() {
        println!("No environments are waiting.");
    }
    for (p, _) in waiting {
        println!(
            "{:<20} Python {:<6} {}  ({})",
            p.id,
            p.python_version.as_deref().unwrap_or("engine"),
            p.packages.join(" "),
            p.reason.lines().next().unwrap_or("")
        );
    }
    Ok(())
}

/// `env approve <id>`: create a proposed environment with Columbus's own
/// uv and register it governed, after the person typed its id on the
/// terminal itself (not stdin).
pub fn approve(id: &str) -> Result<()> {
    use columbus_shared::env_create as create;
    let paths = Paths::from_env();
    let (policy, _) = config::load_policy(&paths)?;
    let p = &policy.environments;
    if !p.allow {
        bail!("organization policy does not allow environments (environments.allow is off), so none can be created");
    }
    let (prop, fingerprint) =
        create::read_proposal(&paths.environments, id).map_err(|e| anyhow!(e))?;
    create::check_proposal(&prop, p).map_err(|e| anyhow!(e))?;
    println!(
        "Your assistant ({}) asks for environment {id}:",
        if prop.client.is_empty() {
            "unknown client"
        } else {
            &prop.client
        }
    );
    println!("  Why: {}", prop.reason);
    println!(
        "  Python: {}",
        prop.python_version
            .as_deref()
            .unwrap_or("the engine's own version")
    );
    for pkg in &prop.packages {
        println!("  {pkg}");
    }
    println!(
        "Columbus will create it in {} with its own uv, from {} only, wheels only (no source builds), with a Python already on this computer, and register it governed (scripts in it run confined, no network, extracts only).",
        paths.envs.join(id).display(),
        p.index_hosts.join(", ")
    );
    let typed = crate::review_cli::confirm_typed(&format!(
        "\nCreate {id}? Type the environment id to confirm: "
    ))?;
    if typed.trim() != id {
        bail!("not confirmed; nothing changed");
    }
    println!("Creating {id} (this downloads the packages; it can take a few minutes)...");
    let env = create::approve(
        &paths.environments,
        &paths.envs,
        p,
        &paths.python,
        id,
        &fingerprint,
        &typed,
        &now(),
    )
    .map_err(|e| anyhow!(e))?;
    println!(
        "Created and registered {} (Python {}, governed).",
        env.id, env.probe.python_version
    );
    Ok(())
}

/// `env decline <id>`: delete a waiting proposal.
pub fn decline(id: &str) -> Result<()> {
    let paths = Paths::from_env();
    columbus_shared::env_create::decline(&paths.environments, id).map_err(|e| anyhow!(e))?;
    println!("Declined {id}.");
    Ok(())
}
