use std::{
    any::{TypeId, type_name},
    fmt::{self, Formatter},
    marker::PhantomData,
    pin::{Pin, pin},
    sync::Arc,
};

use derive_more::derive::{Deref, DerefMut};
use fast::{
    rt::{UnsafeLocal, block_on, spawn, spawn_blocking},
    sync::{barrier::Barrier, channel::Channel},
};

use crate::{component::table::Metatable, query::Params, world::World};

use super::System;

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Deref, DerefMut)]
pub struct Id(pub TypeId);

impl fmt::Debug for Id {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let Self(id) = self;
        let name = &format!("{id:#?}");
        let name = name.trim_start_matches("TypeId(");
        let name = name.trim_end_matches(")");
        f.write_str(name)
    }
}

pub trait Provider {
    type Return;
}

pub trait Func<Input, Marker: Provider>: Send + 'static {
    fn derive(&self) -> Self
    where
        Self: Sized;
    fn execute(self, get: Input) -> impl Future<Output = Marker::Return> + Send
    where
        Self: Sized;
    fn id(&self) -> Id {
        Id(typeid::of::<Self>())
    }
    fn name(&self) -> &'static str {
        type_name::<Self>()
    }
}

pub trait Outcome: Send + 'static {
    fn to_return(self) -> Return;
}

impl Outcome for () {
    fn to_return(self) -> Return {
        todo!()
    }
}

impl Outcome for (i32,) {
    fn to_return(self) -> Return {
        Return {}
    }
}

pub struct Return {}

pub struct Concurrent<T> {
    marker: PhantomData<(T)>,
}
pub struct Blocking<T> {
    marker: PhantomData<(T)>,
}

impl<T> Provider for Blocking<T> {
    type Return = T;
}

impl<Fut: Future> Provider for Concurrent<Fut> {
    type Return = Fut::Output;
}

type Erased = u8;
type PtrChannel = Channel<UnsafeLocal<*mut Erased>, 1>;

pub struct Get<T: Params> {
    wait: Arc<Barrier>,
    get: PtrChannel,
    marker: PhantomData<T>,
}

impl<T: Params> Clone for Get<T> {
    fn clone(&self) -> Self {
        Self {
            wait: self.wait.clone(),
            get: self.get.clone(),
            marker: PhantomData,
        }
    }
}

impl<T: Params> AsyncFnOnce<()> for Get<T> {
    type CallOnceFuture = Getter<T>;

    type Output = T;

    extern "rust-call" fn async_call_once(self, args: ()) -> Self::CallOnceFuture {
        Getter {
            fut: Box::pin(async move {
                self.wait.wait().await;
                let ptr = self.get.recv().await;
                //TODO is this a reference or read
                unsafe { ptr.unwrap().cast::<T>().read() }
            }),
        }
    }
}

pub struct Getter<T: Params> {
    fut: Pin<Box<dyn Future<Output = T> + Send>>,
}

impl<T: Params> Future for Getter<T> {
    type Output = T;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let fut = pin!(&mut self.fut);
        let poll = fut.poll(cx);
        poll
    }
}

pub struct Put {
    wait: Arc<Barrier>,
    binding: Arc<dyn Fn(&mut World) -> Metatable>,
    put: PtrChannel,
}

pub trait Wrap<Input, Marker: Provider>: Func<Input, Marker> {
    fn wrap(self) -> (System, Put);
}

impl<Input: Params, Output: Outcome, F: Func<Input, Blocking<Output>> + Clone>
    Wrap<Input, Blocking<Output>> for F
{
    fn wrap(mut self) -> (System, Put) {
        let (get, put) = bind();
        let system = Box::pin(move || {
            let get = get.clone();
            let func = self.clone();
            let system = block_on(spawn_blocking(move || {
                block_on(async move { func.derive().execute((get)().await).await.to_return() })
            }));
            system
        });
        (system, put)
    }
}

impl<F, Fut: Future<Output = R> + Send, R: Outcome + Send, Input: Params>
    Wrap<Input, Concurrent<Fut>> for F
where
    F: Func<Input, Concurrent<Fut>> + Clone,
{
    fn wrap(mut self) -> (System, Put) {
        let (get, put) = bind();
        let system = Box::pin(move || {
            let get = get.clone();
            let this = self.clone();
            let system = block_on(spawn(async move {
                this.derive().execute((get)().await).await.to_return()
            }));
            system
        });
        (system, put)
    }
}
major_macro::func!();

fn bind<T: Params>() -> (Get<T>, Put) {
    let barrier = Arc::new(Barrier::new(2));
    let get = PtrChannel::default();
    let put = get.clone();
    let binding = Arc::new(T::bind);
    (
        Get {
            wait: barrier.clone(),
            get,
            marker: PhantomData,
        },
        Put {
            wait: barrier,
            binding,
            put,
        },
    )
}
