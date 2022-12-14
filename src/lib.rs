use std::{
    collections::HashMap,
    io::{BufReader, BufWriter},
    sync::Arc,
};

use anyhow::{anyhow, Context as AnyCtx, Result};
use ashfire::target::Target;
use serenity::{
    async_trait,
    framework::standard::{
        macros::{command, group, hook},
        Args, CommandResult, StandardFramework,
    },
    model::prelude::*,
    prelude::*,
};
use shuttle_secrets::SecretStore;
use tracing::{error, info};

struct BotData;

impl TypeMapKey for BotData {
    type Value = Arc<RwLock<HashMap<MessageId, MessageId>>>;
}

struct Bot;

#[async_trait]
impl EventHandler for Bot {
    async fn message_update(&self, ctx: Context, msg: MessageUpdateEvent) {
        let lock = {
            let data_read = ctx.data.read().await;
            let data_lock = data_read
                .get::<BotData>()
                .expect("Expected BotData in TypeMap.");
            let hashmap = data_lock.read().await;
            hashmap.get(&msg.id).copied()
        };

        if let Some(id) = lock {
            info!("A command we replied to was eddited: {}", id);
            // Todo: edit the message with the new result
        }
    }

    async fn ready(&self, _: Context, ready: Ready) {
        info!("{} is connected!", ready.user.name);
    }
}

#[hook]
async fn after_hook(_: &Context, _: &Message, cmd_name: &str, error: CommandResult) {
    //  Print out an error if it happened
    if let Err(why) = error {
        error!("Error in {}: {:?}", cmd_name, why);
    }
}

#[shuttle_service::main]
async fn serenity(
    #[shuttle_secrets::Secrets] secret_store: SecretStore,
) -> shuttle_service::ShuttleSerenity {
    // Get the discord token set in `Secrets.toml`
    let Some(token) = secret_store.get("DISCORD_TOKEN") else {
        return Err(anyhow!("'DISCORD_TOKEN' was not found").into());
    };

    // Set gateway intents, which decides what events the bot will be notified about
    let intents = GatewayIntents::GUILD_MESSAGES | GatewayIntents::MESSAGE_CONTENT;

    let framework = StandardFramework::new()
        .configure(|c| c.with_whitespace(false).prefix("?"))
        .group(&DEFAULT_GROUP)
        .after(after_hook);

    let client = Client::builder(&token, intents)
        .event_handler(Bot)
        .framework(framework)
        .type_map_insert::<BotData>(Arc::new(RwLock::new(HashMap::default())))
        .await
        .expect("Err creating client");

    Ok(client)
}

#[group("default")]
#[commands(eval)]
struct Default;

#[command]
async fn eval(ctx: &Context, msg: &Message, mut _args: Args) -> CommandResult {
    info!("Evaluating message: {}", msg.id);

    let message = match compile(msg) {
        Ok(ok) => ok.to_string(),
        Err(err) => err.to_string(),
    };

    let reply = msg.reply(ctx, message).await?;

    {
        let data_read = ctx.data.read().await;
        let data_lock = data_read
            .get::<BotData>()
            .expect("Expected BotData in TypeMap.");
        let mut hashmap = data_lock.write().await;
        hashmap.insert(msg.id, reply.id);
    };

    Ok(())
}

fn compile(msg: &Message) -> Result<String> {
    let content = &msg.content;
    let trimmed = content
        .strip_prefix("?eval")
        .map(|s| s.trim_start())
        .and_then(|s| s.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .with_context(|| "Failed to parse a code block")?;

    let reader = &mut BufReader::new(trimmed.as_bytes());
    let mut writter = BufWriter::new(vec![]);

    ashfire::compile_buffer(&msg.author.name, reader, &mut writter, Target::Wasi, true)?;

    let output = writter.into_inner()?;
    let out_string = String::from_utf8_lossy(&output).to_string();
    Ok(out_string)
}
