//! Scene hierarchy (GE-031): entities that belong to other entities.
//!
//! A child carries a [`Parent`] component and its [`Transform`] is then
//! local to that parent; an entity without a parent is in world space.
//! [`propagate_transforms`] composes the local transforms down the tree
//! into a [`GlobalTransform`] on every entity that has a `Transform`, which
//! is what [`crate::render::render_world`] draws. It runs at the end of
//! every fixed step ([`crate::ecs::fixed_step`]) and before every render
//! ([`crate::app::EcsGame`], [`crate::headless::Headless`]), so a
//! `GlobalTransform` is at most one edit behind; [`world_transform`]
//! computes the current value on demand.
//!
//! `Parent` is the source of truth. [`Children`] on the parent is kept in
//! step by [`set_parent`] and [`despawn_recursive`] and rebuilt by
//! [`propagate_transforms`], so after undo, redo, or a direct `despawn` it
//! is right again within the frame. Code that must be exact now uses
//! [`child_map`] or [`subtree`], which read the `Parent` components.
//!
//! Composition is translation, rotation, and scale separately
//! ([`Transform::mul_transform`]); a non-uniformly scaled parent with a
//! rotated child loses the shear a matrix product would keep.

use std::collections::HashMap;

use super::{Entity, Resources, World};
use crate::math::Transform;

/// Component on a child: the entity whose space its [`Transform`] is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parent(pub Entity);

/// Component on a parent: its live children, in id order. Present only
/// while the entity has at least one child.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Children(pub Vec<Entity>);

/// Component: the world-space transform, written by
/// [`propagate_transforms`] for every entity with a [`Transform`]. Never
/// edit it by hand; the next propagation overwrites it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GlobalTransform(pub Transform);

/// The id tools and error messages use for an entity.
fn id(entity: Entity) -> u64 {
    entity.to_bits().get()
}

/// `entity`'s parent, if it has a [`Parent`] component naming a live
/// entity other than itself.
pub fn parent_of(world: &World, entity: Entity) -> Option<Entity> {
    world
        .get::<&Parent>(entity)
        .ok()
        .map(|parent| parent.0)
        .filter(|&parent| parent != entity && world.contains(parent))
}

/// Every parent's live children, read from the [`Parent`] components, each
/// list in id order.
pub fn child_map(world: &World) -> HashMap<Entity, Vec<Entity>> {
    let mut map: HashMap<Entity, Vec<Entity>> = HashMap::new();
    for (entity, Parent(parent)) in world.query::<(Entity, &Parent)>().iter() {
        if *parent != entity && world.contains(*parent) {
            map.entry(*parent).or_default().push(entity);
        }
    }
    for children in map.values_mut() {
        children.sort_by_key(|entity| id(*entity));
    }
    map
}

/// `root` followed by its descendants, depth first, siblings in id order.
/// Just `[root]` when it has no children.
pub fn subtree(world: &World, root: Entity) -> Vec<Entity> {
    let map = child_map(world);
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(entity) = stack.pop() {
        out.push(entity);
        if let Some(children) = map.get(&entity) {
            // Reversed so the first child is visited first.
            stack.extend(children.iter().rev());
        }
    }
    out
}

/// `entity`'s local [`Transform`], or the identity if it has none.
fn local_of(world: &World, entity: Entity) -> Transform {
    world
        .get::<&Transform>(entity)
        .map(|transform| *transform)
        .unwrap_or(Transform::IDENTITY)
}

/// `entity`'s world-space transform right now: its local transform
/// composed under every ancestor's. Independent of when
/// [`propagate_transforms`] last ran. An entity without a transform counts
/// as the identity.
pub fn world_transform(world: &World, entity: Entity) -> Transform {
    world_transform_memo(world, entity, &mut HashMap::new())
}

/// [`world_transform`] that remembers every ancestor it resolves in `memo`.
fn world_transform_memo(
    world: &World,
    entity: Entity,
    memo: &mut HashMap<Entity, Transform>,
) -> Transform {
    // Roots are the common case: their world transform is exactly their
    // local one (no composition, so not even a rounding difference).
    let Some(parent) = parent_of(world, entity) else {
        return local_of(world, entity);
    };
    // Walk up to the first ancestor already resolved (or a root),
    // collecting the chain, then compose back down.
    let mut chain = vec![entity];
    let mut current = parent;
    let max_depth = world.len() as usize + 1;
    let base = loop {
        if let Some(known) = memo.get(&current) {
            break *known;
        }
        chain.push(current);
        match parent_of(world, current) {
            // A cycle (impossible through `set_parent`) is cut at the
            // entity where the walk would repeat.
            Some(next) if chain.len() <= max_depth => current = next,
            _ => break Transform::IDENTITY,
        }
    };
    let mut global = base;
    for &ancestor in chain.iter().rev() {
        global = match parent_of(world, ancestor) {
            Some(_) => global.mul_transform(&local_of(world, ancestor)),
            None => local_of(world, ancestor),
        };
        memo.insert(ancestor, global);
    }
    global
}

/// System: writes every entity's world-space transform into its
/// [`GlobalTransform`] (adding the component to entities with a
/// [`Transform`] that lack it) and rebuilds the [`Children`] lists from the
/// [`Parent`] components. Entities without a parent get their local
/// transform exactly.
///
/// Runs automatically at the end of each fixed step and before each
/// render; call it yourself only if you read `GlobalTransform` between an
/// edit and the next frame.
pub fn propagate_transforms(world: &mut World, _res: &mut Resources) {
    let missing: Vec<Entity> = world
        .query::<(Entity, &Transform)>()
        .without::<&GlobalTransform>()
        .iter()
        .map(|(entity, _)| entity)
        .collect();
    for entity in missing {
        let _ = world.insert_one(entity, GlobalTransform::default());
    }

    let map = child_map(world);
    rebuild_children(world, &map);
    if map.is_empty() {
        for (local, global) in world.query_mut::<(&Transform, &mut GlobalTransform)>() {
            global.0 = *local;
        }
        return;
    }

    let entities: Vec<Entity> = world
        .query::<(Entity, &Transform)>()
        .iter()
        .map(|(entity, _)| entity)
        .collect();
    let mut memo = HashMap::new();
    for entity in entities {
        let transform = world_transform_memo(world, entity, &mut memo);
        if let Ok(mut global) = world.get::<&mut GlobalTransform>(entity) {
            global.0 = transform;
        }
    }
}

/// Makes every [`Children`] component equal `map`'s list for its entity,
/// removing the component from entities with no live children.
fn rebuild_children(world: &mut World, map: &HashMap<Entity, Vec<Entity>>) {
    let stale: Vec<Entity> = world
        .query::<(Entity, &Children)>()
        .iter()
        .filter(|(entity, children)| map.get(entity) != Some(&children.0))
        .map(|(entity, _)| entity)
        .collect();
    for entity in stale {
        match map.get(&entity) {
            Some(children) => {
                let _ = world.insert_one(entity, Children(children.clone()));
            }
            None => {
                let _ = world.remove_one::<Children>(entity);
            }
        }
    }
    for (parent, children) in map {
        if !world.satisfies::<&Children>(*parent) {
            let _ = world.insert_one(*parent, Children(children.clone()));
        }
    }
}

/// Makes `parent` the parent of `child`, or removes `child`'s parent with
/// `None`. With `keep_world`, `child`'s local [`Transform`] is rewritten so
/// its world-space transform stays the same (within rounding); otherwise
/// the local transform is kept and the child moves with the new parent.
///
/// Fails, changing nothing, if either entity does not exist, if `parent`
/// is `child` or one of its descendants (a cycle; the error names both
/// ids), or if `keep_world` cannot be honoured because `parent` has a zero
/// scale component.
pub fn set_parent(
    world: &mut World,
    child: Entity,
    parent: Option<Entity>,
    keep_world: bool,
) -> anyhow::Result<()> {
    anyhow::ensure!(world.contains(child), "no entity with id {}", id(child));
    if let Some(parent) = parent {
        anyhow::ensure!(world.contains(parent), "no entity with id {}", id(parent));
        anyhow::ensure!(
            parent != child,
            "entity {} cannot be its own parent",
            id(child)
        );
        let mut current = parent;
        let mut depth = 0;
        while let Some(next) = parent_of(world, current) {
            anyhow::ensure!(
                next != child,
                "cannot make entity {} the parent of entity {}: {} is a descendant of {}",
                id(parent),
                id(child),
                id(parent),
                id(child)
            );
            current = next;
            depth += 1;
            if depth > world.len() {
                break;
            }
        }
    }
    let old = parent_of(world, child);
    if old == parent {
        return Ok(());
    }
    let new_local = keep_world.then(|| {
        let world_space = world_transform(world, child);
        match parent {
            Some(parent) => world_space.relative_to(&world_transform(world, parent)),
            None => world_space,
        }
    });
    if let Some(local) = &new_local {
        anyhow::ensure!(
            local.is_finite(),
            "cannot keep entity {}'s world transform under entity {}: the parent has a zero \
             scale component",
            id(child),
            parent.map_or(0, id)
        );
    }

    if let Some(old) = old {
        remove_child(world, old, child);
    }
    match parent {
        Some(parent) => {
            world.insert_one(child, Parent(parent))?;
            add_child(world, parent, child);
        }
        None => {
            let _ = world.remove_one::<Parent>(child);
        }
    }
    if let Some(local) = new_local {
        world.insert_one(child, local)?;
    }
    Ok(())
}

/// Adds `child` to `parent`'s [`Children`], keeping the list in id order.
fn add_child(world: &mut World, parent: Entity, child: Entity) {
    let mut children = world.remove_one::<Children>(parent).unwrap_or_default();
    if !children.0.contains(&child) {
        children.0.push(child);
        children.0.sort_by_key(|entity| id(*entity));
    }
    let _ = world.insert_one(parent, children);
}

/// Removes `child` from `parent`'s [`Children`], dropping the component
/// when the list becomes empty.
fn remove_child(world: &mut World, parent: Entity, child: Entity) {
    let Ok(mut children) = world.remove_one::<Children>(parent) else {
        return;
    };
    children.0.retain(|entity| *entity != child);
    if !children.0.is_empty() {
        let _ = world.insert_one(parent, children);
    }
}

/// Despawns `entity` and every descendant, and removes `entity` from its
/// parent's [`Children`]. Returns how many entities were removed.
///
/// Fails if `entity` does not exist.
pub fn despawn_recursive(world: &mut World, entity: Entity) -> anyhow::Result<usize> {
    anyhow::ensure!(world.contains(entity), "no entity with id {}", id(entity));
    let doomed = subtree(world, entity);
    if let Some(parent) = parent_of(world, entity) {
        remove_child(world, parent, entity);
    }
    for entity in &doomed {
        world.despawn(*entity)?;
    }
    Ok(doomed.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::{Quat, Vec3};

    const EPS: f32 = 1e-5;

    fn propagate(world: &mut World) {
        propagate_transforms(world, &mut Resources::new());
    }

    fn global(world: &World, entity: Entity) -> Transform {
        world.get::<&GlobalTransform>(entity).unwrap().0
    }

    fn assert_close(a: &Transform, b: &Transform) {
        assert!(
            a.translation.abs_diff_eq(b.translation, EPS),
            "{a:?} vs {b:?}"
        );
        assert!(a.rotation.abs_diff_eq(b.rotation, EPS), "{a:?} vs {b:?}");
        assert!(a.scale.abs_diff_eq(b.scale, EPS), "{a:?} vs {b:?}");
    }

    /// The ticket's case: a parent at (1, 0, 0) rotated 90° about Y with a
    /// child at local (1, 0, 0) puts the child at world (1, 0, -1).
    #[test]
    fn propagation_composes_the_parent_into_the_child() -> anyhow::Result<()> {
        let mut world = World::new();
        let parent = world.spawn((Transform::from_trs(
            Vec3::X,
            Quat::from_rotation_y(90f32.to_radians()),
            Vec3::ONE,
        ),));
        let child = world.spawn((Transform::from_translation(Vec3::X),));
        set_parent(&mut world, child, Some(parent), false)?;
        propagate(&mut world);

        let world_space = global(&world, child);
        assert!(
            world_space
                .translation
                .abs_diff_eq(Vec3::new(1.0, 0.0, -1.0), EPS),
            "{world_space:?}"
        );
        assert_close(&world_space, &world_transform(&world, child));
        // The root's global transform is exactly its local one.
        assert_eq!(global(&world, parent), *world.get::<&Transform>(parent)?);
        assert_eq!(parent_of(&world, child), Some(parent));
        assert_eq!(world.get::<&Children>(parent)?.0, vec![child]);
        Ok(())
    }

    #[test]
    fn propagation_reaches_three_levels_and_tracks_edits() -> anyhow::Result<()> {
        let mut world = World::new();
        let a = world.spawn((Transform::from_translation(Vec3::new(1.0, 0.0, 0.0)),));
        let b = world.spawn((Transform::from_translation(Vec3::new(0.0, 2.0, 0.0)),));
        let c = world.spawn((Transform::from_translation(Vec3::new(0.0, 0.0, 3.0)),));
        set_parent(&mut world, b, Some(a), false)?;
        set_parent(&mut world, c, Some(b), false)?;
        propagate(&mut world);
        assert_eq!(global(&world, c).translation, Vec3::new(1.0, 2.0, 3.0));

        // Moving the root moves the whole subtree at the next propagation.
        world.get::<&mut Transform>(a)?.translation = Vec3::new(10.0, 0.0, 0.0);
        assert_eq!(global(&world, c).translation, Vec3::new(1.0, 2.0, 3.0));
        propagate(&mut world);
        assert_eq!(global(&world, c).translation, Vec3::new(10.0, 2.0, 3.0));
        assert_eq!(subtree(&world, a), vec![a, b, c]);
        Ok(())
    }

    #[test]
    fn keep_world_reparenting_leaves_the_world_transform_unchanged() -> anyhow::Result<()> {
        let mut world = World::new();
        let parent = world.spawn((Transform::from_trs(
            Vec3::new(2.0, 1.0, -3.0),
            Quat::from_euler(glam::EulerRot::XYZ, 0.4, 1.3, -0.7),
            Vec3::splat(2.0),
        ),));
        let child = world.spawn((Transform::from_trs(
            Vec3::new(-1.0, 4.0, 0.5),
            Quat::from_rotation_z(0.6),
            Vec3::new(0.5, 0.5, 0.5),
        ),));
        let before = world_transform(&world, child);

        set_parent(&mut world, child, Some(parent), true)?;
        assert_ne!(
            *world.get::<&Transform>(child)?,
            before,
            "the local changed"
        );
        propagate(&mut world);
        assert_close(&global(&world, child), &before);

        set_parent(&mut world, child, None, true)?;
        propagate(&mut world);
        assert_close(&global(&world, child), &before);
        assert_eq!(parent_of(&world, child), None);
        assert!(world.get::<&Children>(parent).is_err(), "empty lists go");

        // Without keep_world the local transform is kept instead.
        let local = *world.get::<&Transform>(child)?;
        set_parent(&mut world, child, Some(parent), false)?;
        assert_eq!(*world.get::<&Transform>(child)?, local);
        propagate(&mut world);
        assert_close(
            &global(&world, child),
            &world.get::<&Transform>(parent)?.mul_transform(&local),
        );
        Ok(())
    }

    #[test]
    fn set_parent_rejects_cycles_naming_both_ids() -> anyhow::Result<()> {
        let mut world = World::new();
        let a = world.spawn((Transform::IDENTITY,));
        let b = world.spawn((Transform::IDENTITY,));
        let c = world.spawn((Transform::IDENTITY,));
        set_parent(&mut world, b, Some(a), false)?;
        set_parent(&mut world, c, Some(b), false)?;

        let err = set_parent(&mut world, a, Some(c), false).unwrap_err();
        let message = err.to_string();
        assert!(message.contains(&id(a).to_string()), "{message}");
        assert!(message.contains(&id(c).to_string()), "{message}");
        assert_eq!(parent_of(&world, a), None, "nothing changed");

        let err = set_parent(&mut world, a, Some(a), false).unwrap_err();
        assert!(err.to_string().contains("own parent"), "{err}");

        world.despawn(c)?;
        let err = set_parent(&mut world, a, Some(c), false).unwrap_err();
        assert!(err.to_string().contains("no entity"), "{err}");
        Ok(())
    }

    #[test]
    fn keep_world_under_a_zero_scale_parent_fails() -> anyhow::Result<()> {
        let mut world = World::new();
        let flat = world.spawn((Transform::from_trs(
            Vec3::ZERO,
            Quat::IDENTITY,
            Vec3::new(1.0, 0.0, 1.0),
        ),));
        let child = world.spawn((Transform::from_translation(Vec3::Y),));
        let err = set_parent(&mut world, child, Some(flat), true).unwrap_err();
        assert!(err.to_string().contains("zero scale"), "{err}");
        assert_eq!(parent_of(&world, child), None);
        Ok(())
    }

    #[test]
    fn despawn_recursive_removes_the_subtree_and_detaches() -> anyhow::Result<()> {
        let mut world = World::new();
        let root = world.spawn((Transform::IDENTITY,));
        let a = world.spawn((Transform::IDENTITY,));
        let b = world.spawn((Transform::IDENTITY,));
        let c = world.spawn((Transform::IDENTITY,));
        let other = world.spawn((Transform::IDENTITY,));
        set_parent(&mut world, a, Some(root), false)?;
        set_parent(&mut world, b, Some(a), false)?;
        set_parent(&mut world, c, Some(a), false)?;
        set_parent(&mut world, other, Some(root), false)?;
        assert_eq!(world.get::<&Children>(root)?.0, vec![a, other]);

        assert_eq!(despawn_recursive(&mut world, a)?, 3);
        for entity in [a, b, c] {
            assert!(!world.contains(entity));
        }
        assert!(world.contains(other));
        assert_eq!(world.get::<&Children>(root)?.0, vec![other]);
        assert_eq!(despawn_recursive(&mut world, root)?, 2);
        assert_eq!(world.len(), 0);
        assert!(despawn_recursive(&mut world, root).is_err());
        Ok(())
    }

    #[test]
    fn propagation_rebuilds_children_after_direct_changes() -> anyhow::Result<()> {
        let mut world = World::new();
        let root = world.spawn((Transform::IDENTITY,));
        let a = world.spawn((Transform::IDENTITY,));
        let b = world.spawn((Transform::IDENTITY,));
        set_parent(&mut world, a, Some(root), false)?;
        set_parent(&mut world, b, Some(root), false)?;

        // A plain despawn leaves a dead id in the list; an inserted
        // `Parent` is not in it yet. One propagation fixes both.
        world.despawn(a)?;
        let c = world.spawn((Transform::IDENTITY, Parent(root)));
        assert_eq!(world.get::<&Children>(root)?.0, vec![a, b]);
        propagate(&mut world);
        assert_eq!(world.get::<&Children>(root)?.0, vec![b, c]);
        assert_eq!(child_map(&world)[&root], vec![b, c]);

        // A child whose parent died is a root again.
        world.despawn(root)?;
        propagate(&mut world);
        assert_eq!(parent_of(&world, c), None);
        assert_eq!(global(&world, c), Transform::IDENTITY);
        Ok(())
    }

    #[test]
    fn propagation_without_parents_copies_locals_exactly() {
        let mut world = World::new();
        let entities: Vec<Entity> = (0..50)
            .map(|i| {
                world.spawn((Transform::from_trs(
                    Vec3::new(i as f32 * 0.37, (i as f32).sin(), -(i as f32)),
                    Quat::from_rotation_y(i as f32 * 0.1),
                    Vec3::splat(1.0 + i as f32 * 0.01),
                ),))
            })
            .collect();
        propagate(&mut world);
        for entity in entities {
            assert_eq!(
                global(&world, entity),
                *world.get::<&Transform>(entity).unwrap()
            );
        }
    }
}
