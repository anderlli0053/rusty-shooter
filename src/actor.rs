use crate::{
    bot::Bot, character::Character, level::UpdateContext, message::Message, player::Player,
};
use rg3d::{
    core::{
        algebra::Vector3,
        pool::{
            Handle, Pool, PoolIterator, PoolIteratorMut, PoolPairIterator, PoolPairIteratorMut,
        },
        visitor::{Visit, VisitResult, Visitor},
    },
    physics3d::rapier::geometry::ContactEvent,
    scene::Scene,
};
use std::ops::{Deref, DerefMut};

#[allow(clippy::large_enum_variant)]
pub enum Actor {
    Bot(Bot),
    Player(Player),
}

impl Default for Actor {
    fn default() -> Self {
        Actor::Bot(Default::default())
    }
}

macro_rules! static_dispatch {
    ($self:ident, $func:ident, $($args:expr),*) => {
        match $self {
            Actor::Player(v) => v.$func($($args),*),
            Actor::Bot(v) => v.$func($($args),*),
        }
    };
}

impl Actor {
    fn from_id(id: u32) -> Result<Self, String> {
        match id {
            0 => Ok(Actor::Player(Default::default())),
            1 => Ok(Actor::Bot(Default::default())),
            _ => Err(format!("Unknown actor kind {}", id)),
        }
    }

    pub fn id(&self) -> u32 {
        match self {
            Actor::Player(_) => 0,
            Actor::Bot(_) => 1,
        }
    }

    pub fn can_be_removed(&self) -> bool {
        static_dispatch!(self, can_be_removed,)
    }

    pub fn clean_up(&mut self, scene: &mut Scene) {
        static_dispatch!(self, clean_up, scene)
    }
}

impl Deref for Actor {
    type Target = Character;

    fn deref(&self) -> &Self::Target {
        match self {
            Actor::Bot(v) => v,
            Actor::Player(v) => v,
        }
    }
}

impl DerefMut for Actor {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            Actor::Bot(v) => v,
            Actor::Player(v) => v,
        }
    }
}

impl Visit for Actor {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        let mut kind_id = self.id();
        kind_id.visit("KindId", visitor)?;
        if visitor.is_reading() {
            *self = Actor::from_id(kind_id)?;
        }

        match self {
            Actor::Player(player) => player.visit("Data", visitor)?,
            Actor::Bot(bot) => bot.visit("Data", visitor)?,
        }

        visitor.leave_region()
    }
}

// Helper struct that used to hold information about possible target for bots
// it contains all needed information to select suitable target. This is needed
// because of borrowing rules that does not allows to have a mutable reference
// to array element and iterate over array using immutable borrow.
pub struct TargetDescriptor {
    pub handle: Handle<Actor>,
    pub health: f32,
    pub position: Vector3<f32>,
}

#[derive(Default)]
pub struct ActorContainer {
    pool: Pool<Actor>,
    target_descriptors: Vec<TargetDescriptor>,
}

impl ActorContainer {
    pub fn new() -> Self {
        Self {
            pool: Default::default(),
            target_descriptors: Default::default(),
        }
    }

    pub fn add(&mut self, actor: Actor) -> Handle<Actor> {
        self.pool.spawn(actor)
    }

    pub fn get(&self, actor: Handle<Actor>) -> &Actor {
        self.pool.borrow(actor)
    }

    pub fn contains(&self, actor: Handle<Actor>) -> bool {
        self.pool.is_valid_handle(actor)
    }

    pub fn get_mut(&mut self, actor: Handle<Actor>) -> &mut Actor {
        self.pool.borrow_mut(actor)
    }

    pub fn free(&mut self, actor_handle: Handle<Actor>) {
        for actor in self.pool.iter_mut() {
            if let Actor::Bot(bot) = actor {
                bot.on_actor_removed(actor_handle);
            }
        }

        self.pool.free(actor_handle);
    }

    pub fn count(&self) -> u32 {
        self.pool.alive_count()
    }

    pub fn update(&mut self, context: &mut UpdateContext) {
        self.target_descriptors.clear();
        for (handle, actor) in self.pool.pair_iter() {
            self.target_descriptors.push(TargetDescriptor {
                handle,
                health: actor.health,
                position: actor.position(&context.scene.physics),
            });
        }

        for (handle, actor) in self.pool.pair_iter_mut() {
            let is_dead = actor.is_dead();

            match actor {
                Actor::Bot(bot) => bot.update(handle, context, &self.target_descriptors),
                Actor::Player(player) => player.update(context),
            }
            if !is_dead {
                for (item_handle, item) in context.items.pair_iter() {
                    let body = context.scene.physics.bodies.get(&actor.get_body()).unwrap();
                    let distance = (context.scene.graph[item.get_pivot()].global_position()
                        - body.position().translation.vector)
                        .norm();
                    if distance < 1.25 && !item.is_picked_up() {
                        actor
                            .sender
                            .as_ref()
                            .unwrap()
                            .send(Message::PickUpItem {
                                actor: handle,
                                item: item_handle,
                            })
                            .unwrap();
                    }
                }
            }

            if actor.can_be_removed() {
                // Abuse the fact that actor has sender and use it to send message.
                actor
                    .sender
                    .clone()
                    .as_ref()
                    .unwrap()
                    .send(Message::RespawnActor { actor: handle })
                    .unwrap();
            }
        }
    }

    pub fn handle_event(&mut self, contact_event: &ContactEvent, context: &mut UpdateContext) {
        if let &ContactEvent::Started(a, b) = contact_event {
            for actor in self.pool.iter_mut() {
                for jump_pad in context.jump_pads.iter() {
                    let coll_a = context
                        .scene
                        .physics
                        .bodies
                        .handle_map()
                        .key_of(
                            &context
                                .scene
                                .physics
                                .colliders
                                .native_ref(a)
                                .unwrap()
                                .parent()
                                .unwrap(),
                        )
                        .cloned()
                        .unwrap();
                    let coll_b = context
                        .scene
                        .physics
                        .bodies
                        .handle_map()
                        .key_of(
                            &context
                                .scene
                                .physics
                                .colliders
                                .native_ref(b)
                                .unwrap()
                                .parent()
                                .unwrap(),
                        )
                        .cloned()
                        .unwrap();

                    let body = context
                        .scene
                        .physics
                        .bodies
                        .get_mut(&actor.get_body())
                        .unwrap();
                    let capsule_collider = body.colliders()[0];

                    if capsule_collider == a && coll_b == jump_pad.rigid_body()
                        || capsule_collider == b && coll_a == jump_pad.rigid_body()
                    {
                        body.set_linvel(jump_pad.get_force(), true);
                    }
                }
            }
        }
    }

    pub fn iter(&self) -> PoolIterator<Actor> {
        self.pool.iter()
    }

    pub fn pair_iter(&self) -> PoolPairIterator<Actor> {
        self.pool.pair_iter()
    }

    pub fn pair_iter_mut(&mut self) -> PoolPairIteratorMut<Actor> {
        self.pool.pair_iter_mut()
    }

    pub fn iter_mut(&mut self) -> PoolIteratorMut<Actor> {
        self.pool.iter_mut()
    }
}

impl Visit for ActorContainer {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.pool.visit("Pool", visitor)?;

        visitor.leave_region()
    }
}
