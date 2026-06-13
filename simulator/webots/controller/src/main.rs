use anyhow::{Result, anyhow};
use clap::Parser;
use phoxal::api::v1::component::capability::led::Command as LedCommand;
use phoxal::api::v1::component::capability::motor::Command as MotorCommand;
use phoxal::api::v1::simulation::clock::Clock;
use phoxal::api::v1::topic;
use phoxal::bus::Bus;
use phoxal::bus::Error as BusError;
use phoxal::bus::builder::Builder;
use phoxal::bus::typed::TypedTopicSubscriber;
use phoxal::model::component::v1::CapabilityRef;
use phoxal::util::init_tracing;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};
use webots_rs::Webots;

mod capabilities;
mod webots;

#[derive(Clone, Debug, Parser)]
struct Cli {
    #[arg(long = "robot-id", env = "ROBOT_ID")]
    robot_id: String,

    #[arg(long = "robot-namespace", env = "ROBOT_NAMESPACE")]
    robot_namespace: String,

    #[arg(
        long = "robot-router-endpoint",
        env = "ROBOT_ROUTER_ENDPOINT",
        required = true
    )]
    robot_router_endpoint: String,

    #[arg(
        long = "robot-connect-timeout-ms",
        env = "ROBOT_CONNECT_TIMEOUT_MS",
        default_value_t = 5_000_u64
    )]
    robot_connect_timeout_ms: u64,

    #[arg(
        long = "robot-connect-retries",
        env = "ROBOT_CONNECT_RETRIES",
        default_value_t = 60_u32
    )]
    robot_connect_retries: u32,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing()?;

    let cli = Cli::parse();
    info!(
        version = env!("CARGO_PKG_VERSION"),
        robot_id = %cli.robot_id,
        robot_router_endpoint = %cli.robot_router_endpoint,
        robot_namespace = %cli.robot_namespace,
        "webots controller startup context resolved"
    );

    let builder = Builder::new(cli.robot_router_endpoint.clone())
        .with_connect_timeout(std::time::Duration::from_millis(
            cli.robot_connect_timeout_ms,
        ))
        .with_connect_retries(cli.robot_connect_retries)
        .with_prefix(cli.robot_namespace.clone());

    let bus = builder.connect().await?;
    let contract = webots::controller::ControllerContract::load()?;
    let output_dispatcher = webots::output::OutputDispatcher::new(&bus, &contract).await?;

    let command_cache = Arc::new(RwLock::new(
        BTreeMap::<CapabilityRef, webots::Command>::new(),
    ));
    spawn_command_inputs(&bus, &contract, command_cache.clone()).await?;

    let mut webots =
        webots::registry::Webots::new(Webots::new().map_err(|error| anyhow!(error))?, &contract)?;
    let mut shutdown = Box::pin(tokio::signal::ctrl_c());
    let mut step = 0_u64;
    let mut time_ns = 0_u64;
    let dt_ns = webots.dt_ns();

    info!(dt_ns, "webots controller step loop started");

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("received Ctrl+C, shutting down webots controller");
                return Ok(());
            }
            _ = tokio::task::yield_now() => {}
        }

        let next_step = Clock::new(
            0,
            step.saturating_add(1),
            time_ns.saturating_add(dt_ns),
            dt_ns,
        );

        let commands = command_cache
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let demanded_capabilities = output_dispatcher.demanded_capabilities();
        let requested_profiles = output_dispatcher.requested_profiles();

        let Some(outputs) = webots.advance(
            next_step,
            commands,
            &demanded_capabilities,
            &requested_profiles,
        )?
        else {
            info!("webots requested controller shutdown");
            return Ok(());
        };

        output_dispatcher.enqueue(outputs);

        step = next_step.step();
        time_ns = next_step.time_ns();
    }
}

async fn spawn_command_inputs(
    bus: &Bus,
    contract: &webots::controller::ControllerContract,
    command_cache: Arc<RwLock<BTreeMap<CapabilityRef, webots::Command>>>,
) -> Result<()> {
    for component in &contract.components {
        for capability in &component.capabilities {
            match &capability.controller {
                webots::controller::Controller::Motor(config) => {
                    let command_topic = topic::new()
                        .v1()
                        .component(&capability.capability.component_id)
                        .motor(&capability.capability.capability_id)
                        .command();
                    spawn_motor_target_subscription(
                        bus.subscriber(&command_topic).await?,
                        capability.capability.clone(),
                        config.actuator_type,
                        command_cache.clone(),
                    );
                }
                webots::controller::Controller::Led(_) => {
                    let command_topic = topic::new()
                        .v1()
                        .component(&capability.capability.component_id)
                        .led(&capability.capability.capability_id)
                        .command();
                    spawn_led_command_subscription(
                        bus.subscriber(&command_topic).await?,
                        capability.capability.clone(),
                        command_cache.clone(),
                    );
                }
                _ => {}
            }
        }
    }

    Ok(())
}

fn spawn_motor_target_subscription(
    subscriber: TypedTopicSubscriber<MotorCommand>,
    capability: CapabilityRef,
    actuator_type: webots::controller::ActuatorType,
    command_cache: Arc<RwLock<BTreeMap<CapabilityRef, webots::Command>>>,
) {
    tokio::spawn(async move {
        loop {
            let command = match subscriber.recv().await {
                Ok(message) => message.value,
                Err(BusError::TypedDecode(error)) => {
                    warn!(capability = %capability, error = %error, "motor target payload decode failed");
                    continue;
                }
                Err(error) => {
                    warn!(capability = %capability, error = %error, "motor target subscription stopped");
                    return;
                }
            };
            if !valid_motor_command(&command) {
                warn!(capability = %capability, "ignored invalid motor target");
                continue;
            }
            if !supports_motor_command(&command, actuator_type) {
                warn!(capability = %capability, "ignored unsupported motor target");
                continue;
            }
            command_cache.write().await.insert(
                capability.clone(),
                webots::Command::motor(capability.clone(), command),
            );
        }
    });
}

fn spawn_led_command_subscription(
    subscriber: TypedTopicSubscriber<LedCommand>,
    capability: CapabilityRef,
    command_cache: Arc<RwLock<BTreeMap<CapabilityRef, webots::Command>>>,
) {
    tokio::spawn(async move {
        loop {
            let command = match subscriber.recv().await {
                Ok(message) => message.value,
                Err(BusError::TypedDecode(error)) => {
                    warn!(capability = %capability, error = %error, "led command payload decode failed");
                    continue;
                }
                Err(error) => {
                    warn!(capability = %capability, error = %error, "led command subscription stopped");
                    return;
                }
            };
            command_cache.write().await.insert(
                capability.clone(),
                webots::Command::led(capability.clone(), command),
            );
        }
    });
}

fn supports_motor_command(
    command: &MotorCommand,
    actuator_type: webots::controller::ActuatorType,
) -> bool {
    matches!(
        (command, actuator_type),
        (
            MotorCommand::Velocity(_),
            webots::controller::ActuatorType::Velocity
        ) | (
            MotorCommand::Position(_),
            webots::controller::ActuatorType::Position
        ) | (
            MotorCommand::Torque(_),
            webots::controller::ActuatorType::Torque
        )
    )
}

fn valid_motor_command(command: &MotorCommand) -> bool {
    match command {
        MotorCommand::Velocity(value)
        | MotorCommand::Position(value)
        | MotorCommand::Torque(value) => value.is_finite(),
    }
}
