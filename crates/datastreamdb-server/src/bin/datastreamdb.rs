//! `datastreamdb`: supervisor (dev clusters) and version.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "datastreamdb", about = "DataStreamDB supervisor")]
struct Cli {
    /// Subcommand to run.
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run a single-process dev cluster (zero + alpha on loopback).
    Dev {
        /// Base directory for zero/alpha state (`:memory:` for RAM only).
        #[arg(long, default_value = "dev-data")]
        data_dir: PathBuf,
        /// Zero gRPC address.
        #[arg(long, default_value = "127.0.0.1:5080")]
        zero_grpc: String,
        /// Zero HTTP address.
        #[arg(long, default_value = "127.0.0.1:6080")]
        zero_http: String,
        /// Alpha gRPC address.
        #[arg(long, default_value = "127.0.0.1:7080")]
        alpha_grpc: String,
        /// Alpha HTTP address.
        #[arg(long, default_value = "127.0.0.1:8080")]
        alpha_http: String,
    },
    /// Print the version.
    Version,
    /// Manage ACL users, groups, and rules (Go `dgraph acl`).
    Acl {
        /// Alpha gRPC address.
        #[arg(long, short, default_value = "127.0.0.1:7080")]
        alpha: String,
        /// Guardian login (`user=...;password=...;namespace=...`).
        #[arg(long, default_value = "")]
        guardian_creds: String,
        /// Action to run.
        #[command(subcommand)]
        action: AclAction,
    },
}

/// ACL actions (flags mirror Go's).
#[derive(Debug, Subcommand)]
enum AclAction {
    /// Add a user or group.
    Add {
        /// User id to create.
        #[arg(long, short)]
        user: Option<String>,
        /// Group id to create.
        #[arg(long, short)]
        group: Option<String>,
        /// User password (prompted when empty).
        #[arg(long, short)]
        password: Option<String>,
    },
    /// Delete a user or group.
    Del {
        /// User id to delete.
        #[arg(long, short)]
        user: Option<String>,
        /// Group id to delete.
        #[arg(long, short)]
        group: Option<String>,
    },
    /// Modify a user (password/groups) or a group's rule.
    Mod {
        /// User id to change.
        #[arg(long, short)]
        user: Option<String>,
        /// Reset the user's password (prompts).
        #[arg(long, short)]
        new_password: bool,
        /// Groups to set for the user (comma-separated).
        #[arg(long, short, default_value = "datastreamdb-unused-group")]
        group_list: String,
        /// Group whose permission changes.
        #[arg(long, short)]
        group: Option<String>,
        /// Predicate whose rule changes.
        #[arg(long, short)]
        pred: Option<String>,
        /// Permission bits (4 read, 2 write, 1 modify; negative deletes).
        #[arg(long, short, default_value_t = 0)]
        perm: i64,
    },
    /// Show a user or group.
    Info {
        /// User to show.
        #[arg(long, short)]
        user: Option<String>,
        /// Group to show.
        #[arg(long, short)]
        group: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    match Cli::parse().command {
        Command::Version => {
            println!("datastreamdb {}", datastreamdb_server::version());
            Ok(())
        }
        Command::Acl {
            alpha,
            guardian_creds,
            action,
        } => acl(alpha, guardian_creds, action).await,
        Command::Dev {
            data_dir,
            zero_grpc,
            zero_http,
            alpha_grpc,
            alpha_http,
        } => dev(data_dir, &zero_grpc, &zero_http, &alpha_grpc, &alpha_http).await,
    }
}

/// Runs one `datastreamdb acl` action (flag rules mirror Go's).
async fn acl(alpha: String, guardian_creds: String, action: AclAction) -> anyhow::Result<()> {
    use datastreamdb_server::{AclAdmin, parse_guardian_creds};
    let creds = parse_guardian_creds(&guardian_creds)?;
    let mut admin = AclAdmin::connect(&alpha, &creds).await?;
    match action {
        AclAction::Add {
            user,
            group,
            password,
        } => acl_add(&mut admin, user, group, password).await,
        AclAction::Del { user, group } => acl_del(&mut admin, user, group).await,
        AclAction::Mod {
            user,
            new_password,
            group_list,
            group,
            pred,
            perm,
        } => {
            acl_mod(
                &mut admin,
                user,
                new_password,
                group_list,
                group,
                pred,
                perm,
            )
            .await
        }
        AclAction::Info { user, group } => acl_info(&mut admin, user, group).await,
    }
}

/// Adds a user (password prompted when empty) or a group.
async fn acl_add(
    admin: &mut datastreamdb_server::AclAdmin,
    user: Option<String>,
    group: Option<String>,
    password: Option<String>,
) -> anyhow::Result<()> {
    match (user, group) {
        (Some(user), None) => {
            let password = match password {
                Some(password) => password,
                None => rpassword::prompt_password("New password: ")?,
            };
            admin.add_user(&user, &password).await?;
            println!("Created new user with id {user}");
        }
        (None, Some(group)) => {
            if password.is_some() {
                anyhow::bail!("the option --password should not be set");
            }
            admin.add_group(&group).await?;
            println!("Created new group with id {group}");
        }
        _ => anyhow::bail!("one of --user or --group must be specified, but not both"),
    }
    Ok(())
}

/// Deletes a user or group.
async fn acl_del(
    admin: &mut datastreamdb_server::AclAdmin,
    user: Option<String>,
    group: Option<String>,
) -> anyhow::Result<()> {
    match (user, group) {
        (Some(user), None) => {
            admin.del_user(&user).await?;
            println!("Successfully deleted {user:?}");
        }
        (None, Some(group)) => {
            admin.del_group(&group).await?;
            println!("Successfully deleted {group:?}");
        }
        _ => anyhow::bail!("one of --user or --group must be specified, but not both"),
    }
    Ok(())
}

/// Changes a password, sets groups, or edits one rule.
async fn acl_mod(
    admin: &mut datastreamdb_server::AclAdmin,
    user: Option<String>,
    new_password: bool,
    group_list: String,
    group: Option<String>,
    pred: Option<String>,
    perm: i64,
) -> anyhow::Result<()> {
    if let Some(user) = user {
        if group.is_some() || pred.is_some() {
            anyhow::bail!("the options --group/--pred should not be set");
        }
        let defaulted = group_list == "datastreamdb-unused-group";
        if (new_password && !defaulted) || (!new_password && defaulted) {
            anyhow::bail!("one of --new_password or --group_list must be provided, but not both");
        }
        if new_password {
            let password = rpassword::prompt_password("New password: ")?;
            admin.change_password(&user, &password).await?;
            println!("Successfully changed password for {user}");
        } else {
            let groups: Vec<String> = group_list
                .split(',')
                .map(str::trim)
                .filter(|g| !g.is_empty())
                .map(str::to_owned)
                .collect();
            admin.set_user_groups(&user, &groups).await?;
            println!("Successfully modified groups for user {user}.");
        }
    } else {
        if group_list != "datastreamdb-unused-group" || new_password {
            anyhow::bail!("the options --group_list/--new_password should not be set");
        }
        let (Some(group), Some(pred)) = (group, pred) else {
            anyhow::bail!("--group and --pred must both be set");
        };
        admin.chmod(&group, &pred, perm).await?;
        println!("Successfully modified rule for {pred} in {group}.");
    }
    Ok(())
}

/// Shows one user or group.
async fn acl_info(
    admin: &mut datastreamdb_server::AclAdmin,
    user: Option<String>,
    group: Option<String>,
) -> anyhow::Result<()> {
    match (user, group) {
        (Some(user), None) => {
            let Some(found) = admin.user(&user).await? else {
                anyhow::bail!("the user {user:?} does not exist");
            };
            println!("User  : {user}");
            println!("UID   : {}", found.uid);
            for group in &found.groups {
                println!("Group : {:<5}", group.group_id);
            }
        }
        (None, Some(group)) => {
            let Some(found) = admin.group(&group).await? else {
                anyhow::bail!("the group {group} doesn't exist");
            };
            println!("Group: {group}");
            println!("UID  : {}", found.uid);
            println!("ID   : {}", found.group_id);
            let users: Vec<&str> = found
                .users
                .iter()
                .map(|user| user.user_id.as_str())
                .collect();
            println!("Users: {}", users.join(" "));
            for rule in &found.rules {
                println!("ACL  : {} {}", rule.predicate, rule.perm);
            }
        }
        _ => anyhow::bail!("one of --user or --group must be specified, but not both"),
    }
    Ok(())
}

/// Boots zero, waits for it, then boots alpha; serves both until Ctrl-C.
async fn dev(
    data_dir: PathBuf,
    zero_grpc: &str,
    zero_http: &str,
    alpha_grpc: &str,
    alpha_http: &str,
) -> anyhow::Result<()> {
    let in_memory = data_dir.as_os_str() == ":memory:";
    // WALs are memory-mapped files and always need a real directory; in
    // `:memory:` mode they live under the system temp dir (dev only).
    let scratch = std::env::temp_dir().join(format!("datastreamdb-dev-{}", std::process::id()));
    let dir = |name: &str| {
        if in_memory {
            scratch.join(name)
        } else {
            data_dir.join(name)
        }
    };
    let posting = |name: &str| {
        if in_memory {
            PathBuf::from(":memory:")
        } else {
            data_dir.join(name)
        }
    };
    let zero = datastreamdb_server::ZeroServer::boot(datastreamdb_server::ZeroConfig {
        http_addr: zero_http.parse()?,
        grpc_addr: zero_grpc.parse()?,
        my_addr: zero_grpc.to_owned(),
        wal_dir: dir("zw"),
        bootstrap: true,
        join: Vec::new(),
        num_replicas: 1,
        heartbeat_ms: 100,
        election_ms: (1000, 2000),
    })
    .await
    .map_err(|err| anyhow::anyhow!("zero boot: {err}"))?;
    let zero_task = tokio::spawn(async move { zero.serve().await });
    // Zero serves before alpha joins it.
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    let alpha = datastreamdb_frontend::server::AlphaServer::boot(
        datastreamdb_frontend::server::AlphaConfig {
            http_addr: alpha_http.parse()?,
            grpc_addr: alpha_grpc.parse()?,
            my_addr: alpha_grpc.to_owned(),
            zero_addrs: vec![zero_grpc.to_owned()],
            posting_dir: posting("p"),
            wal_dir: dir("w"),
            group_id: 1,
            bootstrap: true,
            heartbeat_ms: 100,
            election_ms: (1000, 2000),
            acl: None,
            audit: datastreamdb_audit::AuditSettings::default(),
            options: datastreamdb_worker::Options {
                posting_dir: posting("p"),
                wal_dir: dir("w"),
                mutations_mode: datastreamdb_worker::MutationsMode::Allow,
                auth_token: String::new(),
                cache_mb: 256,
                remove_on_update: false,
                type_filter_uid_limit: 1000,
                encryption_key: None,
                cdc_conf: String::new(),
            },
        },
    )
    .await
    .map_err(|err| anyhow::anyhow!("alpha boot: {err}"))?;
    let alpha_task = tokio::spawn(async move { alpha.serve().await });

    tracing::info!("dev cluster up (zero {zero_grpc}, alpha {alpha_grpc})");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("shutting down dev cluster");
        }
        out = zero_task => {
            out.map_err(|err| anyhow::anyhow!("{err}"))?
                .map_err(|err| anyhow::anyhow!("zero: {err}"))?;
        }
        out = alpha_task => {
            out.map_err(|err| anyhow::anyhow!("{err}"))?
                .map_err(|err| anyhow::anyhow!("alpha: {err}"))?;
        }
    }
    Ok(())
}
