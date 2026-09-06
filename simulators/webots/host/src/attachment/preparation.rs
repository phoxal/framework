use super::*;

pub(super) struct PreparedRobot {
    pub(super) host: SimulationHostSession,
    pub(super) assets: StagedRobotAssets,
    pub(super) plan: phoxal_simulator_webots_shared::plan::RobotSimulationPlan,
    pub(super) definition: String,
    pub(super) source: String,
}

pub(super) async fn prepare_robot(
    service: &WebotsAttachments,
    host: SimulationHostSession,
    execution: ExecutionId,
    supervisor_endpoint: &str,
    initial_pose: phoxal::model::structure::Pose,
    cancellation: &OperationCancellation,
) -> Result<PreparedRobot> {
    let staged = StagedRobotAssets::new(&service.project_root, execution);
    let preparation = async {
        cancellation.check()?;
        ensure!(
            host.execution() == execution,
            "session endpoint resolved execution {}, expected {execution}",
            host.execution()
        );
        let full_plan = required_assets(host.robot())?;
        let mut assets = BTreeMap::new();
        for id in full_plan.required_assets() {
            assets.insert(
                id.clone(),
                host.assets()
                    .read(id)
                    .await
                    .with_context(|| format!("failed to preflight asset {id}"))?,
            );
            cancellation.check()?;
        }
        let materials = assets.iter().try_fold(
            std::collections::BTreeSet::new(),
            |mut dependencies, (id, bytes)| {
                dependencies.extend(crate::obj::material_dependencies(id, bytes)?);
                Ok::<_, anyhow::Error>(dependencies)
            },
        )?;
        for id in materials {
            if let std::collections::btree_map::Entry::Vacant(entry) = assets.entry(id) {
                let bytes = host.assets().read(entry.key()).await.with_context(|| {
                    format!("failed to preflight mesh material {}", entry.key())
                })?;
                entry.insert(bytes);
                cancellation.check()?;
            }
        }
        let mut collision_assets = host
            .robot()
            .structure()
            .links()
            .flat_map(|link| link.collisions())
            .filter_map(|collision| collision.geometry().asset_id().cloned())
            .collect::<std::collections::BTreeSet<_>>();
        for component in host.robot().components() {
            collision_assets.extend(
                component
                    .component_type()
                    .structure()
                    .links()
                    .flat_map(|link| link.collisions())
                    .filter_map(|collision| collision.geometry().asset_id().cloned()),
            );
        }
        for collision in collision_assets {
            crate::obj::decode(&collision, &assets)?
                .validate_collision()
                .with_context(|| {
                    format!("Robot collision asset {collision} exceeds the accepted Webots subset")
                })?;
        }
        let step_ms = i32::try_from(service.world.time_step_ns() / 1_000_000)
            .context("world time step does not fit Webots milliseconds")?;
        let plan = lower_robot_plan(host.robot(), &full_plan, step_ms, |id| {
            assets
                .get(id)
                .cloned()
                .ok_or_else(|| format!("asset {id} was not prefetched"))
        })?;
        staged.stage(&assets).await?;
        cancellation.check()?;
        let definition = robot_definition(execution);
        let source = render_robot(
            host.robot(),
            &plan,
            &assets,
            execution,
            initial_pose,
            supervisor_endpoint,
            service.native.endpoint(),
        )
        .context("failed to render the admitted native Robot")?;
        validate_robot_import(&definition, &source)
            .context("generated Robot exceeds the native import budget")?;
        let _: webots_proto_ast::Proto = source
            .parse()
            .context("generated native Robot did not parse as R2025a VRML")?;
        cancellation.check()?;
        Ok::<_, anyhow::Error>((plan, definition, source))
    }
    .await;
    match preparation {
        Ok((plan, definition, source)) => Ok(PreparedRobot {
            host,
            assets: staged,
            plan,
            definition,
            source,
        }),
        Err(error) => {
            let cleanup = staged.cleanup().await;
            let close = host.close().await;
            if cleanup.is_err() || close.is_err() {
                Err(error.context(format!(
                    "preparation cleanup: {cleanup:?}; host close: {close:?}"
                )))
            } else {
                Err(error)
            }
        }
    }
}

pub(super) fn ensure_idempotent_request(
    execution: ExecutionId,
    existing_spawn: &SpawnId,
    existing_endpoint: &str,
    requested_spawn: &SpawnId,
    requested_endpoint: &str,
) -> Result<()> {
    ensure!(
        existing_spawn == requested_spawn,
        "idempotent execution {execution} retry changed its resolved spawn"
    );
    ensure!(
        existing_endpoint == requested_endpoint,
        "idempotent execution {execution} retry changed its supervisor endpoint"
    );
    Ok(())
}

pub(super) fn ensure_attach_slot(
    members: &[WorldMember],
    execution: ExecutionId,
    spawn: &SpawnId,
) -> Result<()> {
    ensure!(
        !members.iter().any(|member| member.execution == execution),
        "world state already contains execution {execution} without a retained host session"
    );
    ensure!(
        !members.iter().any(|member| &member.spawn == spawn),
        "spawn point '{spawn}' is already occupied"
    );
    Ok(())
}

pub(super) fn resolve_spawn(
    world: &World,
    requested: Option<SpawnId>,
) -> Result<(SpawnId, phoxal::model::structure::Pose)> {
    let spawns = world.spawn_points().collect::<Vec<_>>();
    match requested {
        Some(requested) => spawns
            .into_iter()
            .find(|(id, _)| **id == requested)
            .map(|(id, pose)| (id.clone(), pose))
            .with_context(|| format!("world has no spawn point '{requested}'")),
        None => {
            let [(id, pose)] = spawns.as_slice() else {
                anyhow::bail!(
                    "spawn may be omitted only when the world has exactly one authored spawn point"
                );
            };
            Ok(((*id).clone(), *pose))
        }
    }
}

pub(super) async fn wait_for_controller(
    native: &HostServer,
    execution: ExecutionId,
    cancellation: &OperationCancellation,
) -> Result<phoxal::identity::ProducerId> {
    let deadline = tokio::time::Instant::now() + CONTROLLER_READY_TIMEOUT;
    loop {
        cancellation.check()?;
        if let Some(controller) = native.robot_controller(execution) {
            return Ok(controller);
        }
        if let NativeWorldLifecycle::Failed(failure) = native.snapshot().lifecycle() {
            anyhow::bail!("native world failed while Robot started: {failure:?}");
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "Robot controller did not become ready within {CONTROLLER_READY_TIMEOUT:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

pub(super) async fn wait_for_active_ack(
    native: &HostServer,
    execution: ExecutionId,
    revision: u64,
    cancellation: &OperationCancellation,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + CONTROLLER_READY_TIMEOUT;
    loop {
        cancellation.check()?;
        if native.robot_active_revision(execution) == Some(revision) {
            return Ok(());
        }
        if let NativeWorldLifecycle::Failed(failure) = native.snapshot().lifecycle() {
            anyhow::bail!("native world failed before Robot acknowledged Active: {failure:?}");
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "Robot controller did not acknowledge Active revision {revision} within {CONTROLLER_READY_TIMEOUT:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
