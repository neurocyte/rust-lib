use std::{cell::UnsafeCell, marker::PhantomData};

use super::{IntoProcess, IntoProcessLink, Process};
use crate::{
    environment::{params_to_vec, Param},
    host_api,
    serializer::{Bincode, Serializer},
    LunaticError, Mailbox, Resource, Tag,
};

pub trait HandleMessage<M, S = Bincode>
where
    S: Serializer<M>,
{
    fn handle(&mut self, message: M);
}

pub trait HandleRequest<M, S = Bincode>
where
    S: Serializer<M>,
{
    type Result;

    fn handle(&mut self, request: M) -> Self::Result;
}

pub trait Message<M, S>
where
    S: Serializer<M>,
{
    fn send(&self, message: M);
}

pub trait Request<M, S = Bincode>
where
    S: Serializer<M>,
{
    type Result;

    fn request(&self, request: M) -> Self::Result;
}

/// A [`GenericServer`] is a process spawned from a state that implements the `HandleMessage` &
/// `HandleRequest` traits, allowing it to handle different types of messages sent to it.
pub struct GenericServer<T> {
    id: u64,
    // If set to true, the host call `lunatic::process::drop_process` will not be executed on drop.
    consumed: UnsafeCell<bool>,
    serializer_type: PhantomData<T>,
}

impl<M, S, T> Message<M, S> for GenericServer<T>
where
    T: HandleMessage<M, S>,
    S: Serializer<M>,
{
    fn send(&self, message: M) {
        fn unpacker<TU, MU, SU>(this: &mut TU)
        where
            TU: HandleMessage<MU, SU>,
            SU: Serializer<MU>,
        {
            let message: MU = SU::decode().unwrap();
            <TU as HandleMessage<MU, SU>>::handle(this, message);
        }

        // Create new message buffer.
        unsafe { host_api::message::create_data(1, 0) };
        // First encode the handler inside the message buffer.
        let handler = unpacker::<T, M, S> as i32;
        let handler_message = Sendable::Message(handler);
        Bincode::encode(&handler_message).unwrap();
        // Then the message itself.
        S::encode(&message).unwrap();
        // Send the message
        unsafe { host_api::message::send(self.id) };
    }
}

impl<T> Resource for GenericServer<T> {
    fn id(&self) -> u64 {
        self.id
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
enum Sendable {
    Message(i32),
    // The process type can't be carried over as a generic and is set here to `()`, but overwritten
    // at the time of returning with the correct type.
    Request(i32, Process<()>),
}

impl<M, S, T> Request<M, S> for GenericServer<T>
where
    T: HandleRequest<M, S>,
    S: Serializer<M> + Serializer<<T as HandleRequest<M, S>>::Result>,
{
    type Result = <T as HandleRequest<M, S>>::Result;

    fn request(&self, request: M) -> Self::Result {
        fn unpacker<TU, MU, SU>(
            this: &mut TU,
            sender: Process<<TU as HandleRequest<MU, SU>>::Result, SU>,
        ) where
            TU: HandleRequest<MU, SU>,
            SU: Serializer<MU> + Serializer<<TU as HandleRequest<MU, SU>>::Result>,
        {
            // Get content out of message
            let message: MU = SU::decode().unwrap();
            let result = <TU as HandleRequest<MU, SU>>::handle(this, message);
            // Get tag out of message
            let tag = unsafe { host_api::message::get_tag() };
            let tag = Tag::from(tag);
            sender.tag_send(tag, result);
        }

        let tag = Tag::new();
        // Create new message buffer.
        unsafe { host_api::message::create_data(tag.id(), 0) };
        // Create reference to self
        let this_id = unsafe { host_api::process::this() };
        let this_proc: Process<()> = unsafe { Process::from(this_id) };
        // First encode the handler inside the message buffer.
        let handler = unpacker::<T, M, S> as i32;
        let handler_message = Sendable::Request(handler, this_proc);
        Bincode::encode(&handler_message).unwrap();
        // Then the message itself.
        S::encode(&request).unwrap();
        // Send it & wait on a response!
        unsafe { host_api::message::send_receive_skip_search(self.id, 0) };
        S::decode().unwrap()
    }
}

impl<T> GenericServer<T> {
    /// Construct a process from a raw ID.
    pub unsafe fn from(id: u64) -> Self {
        GenericServer {
            id,
            consumed: UnsafeCell::new(false),
            serializer_type: PhantomData,
        }
    }

    /// Returns a globally unique process ID.
    pub fn uuid(&self) -> u128 {
        let mut uuid: [u8; 16] = [0; 16];
        unsafe { host_api::process::id(self.id, &mut uuid as *mut [u8; 16]) };
        u128::from_le_bytes(uuid)
    }

    fn send_init<C>(&self, message: C)
    where
        C: serde::Serialize + serde::de::DeserializeOwned,
    {
        // Create new message.
        unsafe { host_api::message::create_data(1, 0) };
        // During serialization resources will add themself to the message.
        Bincode::encode(&message).unwrap();
        // Send it!
        unsafe { host_api::message::send(self.id) };
    }

    /// Marks the process as consumed.
    ///
    /// Consumed processes don't call the `lunatic::process::drop_process` host function when they
    /// are dropped. This characteristic is useful when implementing serializers for processes.
    /// Serializers will move the process out of the local state into the message scratch buffer
    /// and they can't be dropped from the local state anymore.
    pub unsafe fn consume(&self) {
        *self.consumed.get() = true;
    }
}

impl<T> IntoProcess<T> for GenericServer<T>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    type Handler = fn(state: &mut T);

    fn spawn(
        module: Option<u64>,
        state: T,
        init: Self::Handler,
    ) -> Result<GenericServer<T>, LunaticError>
    where
        Self: Sized,
    {
        spawn(module, false, state, init)
    }
}

impl<T> IntoProcessLink<T> for GenericServer<T>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    type Handler = fn(state: &mut T);

    fn spawn_link(
        module: Option<u64>,
        state: T,
        init: Self::Handler,
    ) -> Result<GenericServer<T>, LunaticError>
    where
        Self: Sized,
    {
        spawn(module, true, state, init)
    }
}

// `spawn` performs a low level dance that will turn a high level rust function and state into a
// correct lunatic server.
//
// For more info on how this function works, read the explanation inside super::process::spawn.
fn spawn<T>(
    module: Option<u64>,
    link: bool,
    state: T,
    init: fn(state: &mut T),
) -> Result<GenericServer<T>, LunaticError>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let (type_helper, init) = (type_helper_wrapper::<T> as i32, init as i32);

    let params = params_to_vec(&[Param::I32(type_helper), Param::I32(init)]);
    let mut id = 0;
    let func = "_lunatic_spawn_gen_server_by_index";
    let link = match link {
        // TODO: Do we want to be notified with the right tag once the link dies?
        //       I assume not, because only supervisors can use this information and we can't spawn
        //       this kind of processes from supervisors.
        true => 1,
        false => 0,
    };
    let result = unsafe {
        match module {
            Some(module_id) => host_api::process::spawn(
                link,
                module_id,
                func.as_ptr(),
                func.len(),
                params.as_ptr(),
                params.len(),
                &mut id,
            ),
            None => host_api::process::inherit_spawn(
                link,
                func.as_ptr(),
                func.len(),
                params.as_ptr(),
                params.len(),
                &mut id,
            ),
        }
    };
    if result == 0 {
        // If the captured variable is of size 0, we don't need to send it to another process.
        if std::mem::size_of::<T>() == 0 {
            Ok(GenericServer {
                id,
                consumed: UnsafeCell::new(false),
                serializer_type: PhantomData,
            })
        } else {
            let child = GenericServer::<T> {
                id,
                consumed: UnsafeCell::new(false),
                serializer_type: PhantomData,
            };
            child.send_init(state);
            Ok(child)
        }
    } else {
        Err(LunaticError::from(id))
    }
}

// Type helper
fn type_helper_wrapper<T>(function: usize)
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    // If the captured variable is of size 0, don't wait on it.
    let mut state = if std::mem::size_of::<T>() == 0 {
        unsafe { std::mem::MaybeUninit::<T>::zeroed().assume_init() }
    } else {
        unsafe { Mailbox::<T, Bincode>::new() }.receive()
    };

    // Run the init function before anything else
    let init: fn(state: &mut T) = unsafe { std::mem::transmute(function) };
    init(&mut state);

    let mailbox: Mailbox<Sendable, Bincode> = unsafe { Mailbox::new() };

    // Run server forever and respond to requests.
    loop {
        let dispatcher = mailbox.tag_receive(None);
        match dispatcher {
            Sendable::Message(handler) => {
                let handler: fn(state: &mut T) = unsafe { std::mem::transmute(handler) };
                handler(&mut state);
            }
            Sendable::Request(handler, sender) => {
                let handler: fn(state: &mut T, sender: Process<()>) =
                    unsafe { std::mem::transmute(handler) };
                handler(&mut state, sender);
            }
        }
    }
}

#[export_name = "_lunatic_spawn_gen_server_by_index"]
extern "C" fn _lunatic_spawn_gen_server_by_index(type_helper: usize, function: usize) {
    let type_helper: fn(usize) = unsafe { std::mem::transmute(type_helper) };
    type_helper(function);
}

// Processes are equal if their UUID is equal.
impl<T> PartialEq for GenericServer<T> {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl<T> std::fmt::Debug for GenericServer<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Process").field("uuid", &self.id()).finish()
    }
}

impl<T> Clone for GenericServer<T> {
    fn clone(&self) -> Self {
        let id = unsafe { host_api::process::clone_process(self.id) };
        unsafe { GenericServer::from(id) }
    }
}

impl<T> Drop for GenericServer<T> {
    fn drop(&mut self) {
        // Only drop a process if it's not already consumed.
        if unsafe { !*self.consumed.get() } {
            unsafe { host_api::process::drop_process(self.id) };
        }
    }
}

impl<T> serde::Serialize for GenericServer<T> {
    fn serialize<A>(&self, serializer: A) -> Result<A::Ok, A::Error>
    where
        A: serde::Serializer,
    {
        // Mark process as consumed.
        unsafe { self.consume() };

        let index = unsafe { host_api::message::push_process(self.id) };
        serializer.serialize_u64(index)
    }
}

struct GenericServerVisitor<T> {
    _phantom: PhantomData<T>,
}

impl<'de, T> serde::de::Visitor<'de> for GenericServerVisitor<T> {
    type Value = GenericServer<T>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("an u64 index")
    }

    fn visit_u64<E>(self, index: u64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        let id = unsafe { host_api::message::take_process(index) };
        Ok(unsafe { GenericServer::from(id) })
    }
}

impl<'de, T> serde::de::Deserialize<'de> for GenericServer<T> {
    fn deserialize<D>(deserializer: D) -> Result<GenericServer<T>, D::Error>
    where
        D: serde::de::Deserializer<'de>,
    {
        deserializer.deserialize_u64(GenericServerVisitor {
            _phantom: PhantomData {},
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::process::{sleep, spawn, spawn_link};

    #[derive(serde::Serialize, serde::Deserialize)]
    struct TestServer(i32);

    impl HandleMessage<i32> for TestServer {
        fn handle(&mut self, message: i32) {
            self.0 += message;
        }
    }

    impl HandleRequest<()> for TestServer {
        type Result = i32;

        fn handle(&mut self, _: ()) -> Self::Result {
            self.0
        }
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Panic;

    impl HandleMessage<Panic> for TestServer {
        fn handle(&mut self, _: Panic) {
            panic!("fail");
        }
    }

    #[test]
    fn spawn_test() {
        let child = spawn::<GenericServer<_>, _>(TestServer(0), |_state| {}).unwrap();
        child.send(33);
        child.send(55);
        let result = child.request(());
        assert_eq!(result, 88);

        sleep(Duration::from_millis(100));
    }

    #[test]
    fn spawn_link_test() {
        // There is no real way of testing traps for now, at least not until this is resolved:
        // https://github.com/lunatic-solutions/rust-lib/issues/8
        // A manual log output observation is necessary her to check if both processes failed.
        spawn::<GenericServer<()>, _>((), |_| {
            let child = spawn_link::<GenericServer<_>, _>(TestServer(0), |_| {}).unwrap();
            // Trigger failure
            child.send(Panic);
            // This process should fails too before 100ms
            sleep(Duration::from_millis(100));
        })
        .unwrap();
        sleep(Duration::from_millis(100));
    }
}