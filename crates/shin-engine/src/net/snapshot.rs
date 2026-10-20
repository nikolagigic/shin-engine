//! Turning the world into a [`Snapshot`] ([`capture`], on the server) and
//! a snapshot back into entities ([`apply`], on a client).
//!
//! What is replicated per entity: its [`NetId`], parent link, [`Name`],
//! [`Transform`], mesh, colour, and texture (by [`AssetSource`] string),
//! and [`Owner`]. Scripts, sounds, and physics are not: they run on the
//! server, and clients see their effect through the transforms.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Context as _;

use super::protocol::{EntitySnap, Snapshot};
use super::{NetId, NetIds, Owner, Replicated};
use crate::asset::{AssetSource, Assets};
use crate::ecs::{
    Entity, Name, Resources, World, despawn_recursive, parent_of, set_parent, subtree,
    world_transform,
};
use crate::math::Transform;
use crate::render::{MeshHandle, MeshInstance, RenderState, TextureHandle};
use crate::scene::TransformDesc;

/// Captures every entity with [`Replicated`] and a [`Transform`] at `tick`,
/// in [`NetId`] order.
///
/// Uses the [`NetIds`] resource (inserting one if missing): entities
/// without an id get a fresh one, assigned in entity id order. Entities
/// that had an id but are gone or no longer replicated are listed in
/// [`Snapshot::removed`] and forgotten, so the next capture does not list
/// them again.
///
/// A parent link is kept when the parent is replicated too; an entity
/// whose parent is not replicated is sent as a root at its world
/// transform. Meshes and textures are sent by their [`AssetSource`]; one
/// added from code without a source (see [`Assets`]) is left out, with a
/// debug log.
pub fn capture(world: &World, res: &mut Resources, tick: u64) -> Snapshot {
    let mut ids = take_ids(res);
    let snapshot = capture_with(world, &mut ids, res.get::<RenderState>(), tick);
    res.insert(ids);
    snapshot
}

fn capture_with(
    world: &World,
    ids: &mut NetIds,
    state: Option<&RenderState>,
    tick: u64,
) -> Snapshot {
    let mut replicated: Vec<Entity> = world
        .query::<(Entity, &Replicated, &Transform)>()
        .iter()
        .map(|(entity, ..)| entity)
        .collect();
    replicated.sort_by_key(|entity| entity.to_bits());
    let members: HashSet<Entity> = replicated.iter().copied().collect();
    for &entity in &replicated {
        ids.assign(entity);
    }
    let mut removed: Vec<NetId> = ids
        .iter()
        .filter(|(entity, _)| !members.contains(entity))
        .map(|(_, id)| id)
        .collect();
    removed.sort();
    for &id in &removed {
        ids.remove_id(id);
    }

    let mut entities: Vec<EntitySnap> = replicated
        .iter()
        .map(|&entity| {
            let id = ids.id(entity).expect("assigned above");
            let parent = parent_of(world, entity);
            let net_parent = parent
                .filter(|p| members.contains(p))
                .and_then(|p| ids.id(p));
            let transform = match (parent, net_parent) {
                (Some(_), None) => world_transform(world, entity),
                _ => *world.get::<&Transform>(entity).expect("queried above"),
            };
            let (mesh, color, texture) = match world.get::<&MeshInstance>(entity) {
                Ok(instance) => draw_sources(state, &instance, id),
                Err(_) => (None, None, None),
            };
            EntitySnap {
                id,
                parent: net_parent,
                name: world.get::<&Name>(entity).ok().map(|n| n.0.clone()),
                transform: TransformDesc::from_transform(&transform),
                mesh,
                color,
                texture,
                owner: world.get::<&Owner>(entity).ok().map(|o| o.0),
            }
        })
        .collect();
    entities.sort_by_key(|snap| snap.id);
    Snapshot {
        tick,
        entities,
        removed,
    }
}

/// The mesh source, colour, and texture source `instance` draws with. A
/// handle without a source sends no mesh (or no texture).
fn draw_sources(
    state: Option<&RenderState>,
    instance: &MeshInstance,
    id: NetId,
) -> (Option<String>, Option<[f32; 4]>, Option<String>) {
    let Some(mesh) = state.and_then(|s| s.registry.source(instance.mesh)) else {
        log::debug!(
            "net: entity {id:?} draws mesh handle {} with no asset source; not replicated",
            instance.mesh.0
        );
        return (None, None, None);
    };
    let texture = instance.texture.and_then(|handle| {
        let source = state.and_then(|s| s.textures.source(handle));
        if source.is_none() {
            log::debug!(
                "net: entity {id:?} uses texture handle {} with no asset source; not replicated",
                handle.0
            );
        }
        source.map(ToString::to_string)
    });
    (Some(mesh.to_string()), Some(instance.color), texture)
}

/// Applies `snap` to `world`: spawns entities it lists that have no local
/// counterpart yet, updates those that do, then despawns every entity in
/// [`Snapshot::removed`] together with its descendants. Entities the
/// snapshot does not mention are left alone, and applying the same
/// snapshot twice changes nothing the second time.
///
/// Uses the [`NetIds`] resource (inserting one if missing) to match ids to
/// entities. Spawned entities get [`Replicated`]. A field the snapshot
/// leaves empty removes the component (a `mesh: None` entity loses its
/// [`MeshInstance`]); a parent the client does not know makes the entity
/// a root, with a warning.
///
/// Meshes and textures are resolved through the [`Assets`] resource and
/// uploaded into the [`RenderState`] registries, all before the world is
/// touched, so a snapshot with a source that does not load fails (naming
/// it) and changes nothing.
pub fn apply(
    world: &mut World,
    res: &mut Resources,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    snap: &Snapshot,
) -> anyhow::Result<()> {
    let draws = resolve_draws(res, device, queue, snap)?;
    let mut ids = take_ids(res);
    let result = apply_with(world, &mut ids, snap, draws);
    res.insert(ids);
    result
}

/// What an entity draws: mesh, colour, optional texture.
type Draw = (MeshHandle, [f32; 4], Option<TextureHandle>);

/// The handles every entity of `snap` draws with, in order, uploading
/// sources as needed.
fn resolve_draws(
    res: &mut Resources,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    snap: &Snapshot,
) -> anyhow::Result<Vec<Option<Draw>>> {
    // Resources lend one value mutably at a time, so `Assets` is moved out
    // for the duration and always put back.
    let slot = res.get_mut::<Assets>().context("no Assets resource")?;
    let mut assets = std::mem::replace(slot, Assets::new(PathBuf::new()));
    let result = (|| {
        let state = res
            .get_mut::<RenderState>()
            .context("no RenderState resource")?;
        snap.entities
            .iter()
            .map(|entity| {
                let Some(text) = &entity.mesh else {
                    return Ok(None);
                };
                let id = entity.id.0;
                let mesh = AssetSource::parse(text)
                    .and_then(|source| assets.mesh(device, &mut state.registry, &source))
                    .with_context(|| format!("net id {id}: mesh {text:?} could not be loaded"))?;
                let texture = entity
                    .texture
                    .as_ref()
                    .map(|text| {
                        AssetSource::parse(text)
                            .and_then(|source| {
                                assets.texture(device, queue, &mut state.textures, &source)
                            })
                            .with_context(|| {
                                format!("net id {id}: texture {text:?} could not be loaded")
                            })
                    })
                    .transpose()?;
                Ok(Some((mesh, entity.color.unwrap_or([1.0; 4]), texture)))
            })
            .collect()
    })();
    res.insert(assets);
    result
}

fn apply_with(
    world: &mut World,
    ids: &mut NetIds,
    snap: &Snapshot,
    draws: Vec<Option<Draw>>,
) -> anyhow::Result<()> {
    // Spawn or update every listed entity.
    let mut entities = Vec::with_capacity(snap.entities.len());
    for (desc, draw) in snap.entities.iter().zip(draws) {
        let entity = match ids.entity(desc.id).filter(|&e| world.contains(e)) {
            Some(entity) => entity,
            None => {
                let entity = world.spawn((Replicated,));
                ids.insert(entity, desc.id);
                entity
            }
        };
        set(world, entity, Some(desc.transform.to_transform()))?;
        set(world, entity, desc.name.clone().map(Name))?;
        set(world, entity, desc.owner.map(Owner))?;
        let instance = draw.map(|(mesh, color, texture)| MeshInstance {
            mesh,
            color,
            texture,
        });
        let current = world.get::<&MeshInstance>(entity).ok().map(|i| *i);
        if !same_instance(current.as_ref(), instance.as_ref()) {
            match instance {
                Some(instance) => world.insert_one(entity, instance)?,
                None => drop(world.remove_one::<MeshInstance>(entity)),
            }
        }
        entities.push(entity);
    }

    // Then link parents, now that every listed entity exists. The
    // snapshot's transforms are already local to their parents.
    for (desc, &entity) in snap.entities.iter().zip(&entities) {
        let parent = desc.parent.and_then(|id| {
            let found = ids.entity(id).filter(|&e| world.contains(e));
            if found.is_none() {
                log::warn!("net: {:?} has unknown parent {id:?}; made a root", desc.id);
            }
            found
        });
        if parent_of(world, entity) != parent
            && let Err(err) = set_parent(world, entity, parent, false)
        {
            log::warn!("net: cannot parent {:?}: {err:#}", desc.id);
        }
    }

    // Finally despawn what was removed, with its descendants.
    for &id in &snap.removed {
        let Some(entity) = ids.remove_id(id).filter(|&e| world.contains(e)) else {
            continue;
        };
        for doomed in subtree(world, entity) {
            if let Some(id) = ids.id(doomed) {
                ids.remove_id(id);
            }
        }
        despawn_recursive(world, entity)?;
    }
    Ok(())
}

/// Sets `entity`'s `T` to `value` (removing it for `None`), writing only
/// if it differs.
fn set<T: hecs::Component + PartialEq>(
    world: &mut World,
    entity: Entity,
    value: Option<T>,
) -> anyhow::Result<()> {
    let same = match (world.get::<&T>(entity).ok(), &value) {
        (Some(current), Some(value)) => *current == *value,
        (None, None) => true,
        _ => false,
    };
    if !same {
        match value {
            Some(value) => world.insert_one(entity, value)?,
            None => drop(world.remove_one::<T>(entity)),
        }
    }
    Ok(())
}

/// Whether two optional instances draw the same thing.
fn same_instance(a: Option<&MeshInstance>, b: Option<&MeshInstance>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.mesh == b.mesh && a.color == b.color && a.texture == b.texture,
        (None, None) => true,
        _ => false,
    }
}

/// The [`NetIds`] resource, moved out of `res` (empty if there was none).
/// Callers put it back.
fn take_ids(res: &mut Resources) -> NetIds {
    res.get_mut::<NetIds>()
        .map(std::mem::take)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
