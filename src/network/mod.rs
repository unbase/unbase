extern crate linked_hash_map;

pub mod transport;
pub mod slabref;
mod packet;

pub use self::slabref::{SlabRef, SlabPresence, SlabAnticipatedLifetime};
pub use self::transport::{Transport};
use self::transport::*;

use std::sync::{Arc, Weak, Mutex};
use std::fmt;
use slab::{Slab,WeakSlab,SlabId};
use memorefhead::MemoRefHead;

struct NetworkInternals {
    next_slab_id: u32,
    slabs:     Vec<WeakSlab>,
    slab_refs: Vec<SlabRef>,
    transports: Vec<Box<Transport + Send + Sync>>,
    root_index_seed: Option<MemoRefHead>
}

pub struct NetworkShared {
    internals: Mutex<NetworkInternals>
}

#[derive(Clone)]
pub struct Network {
    shared: Arc<NetworkShared>
}
pub struct WeakNetwork {
    shared: Weak<NetworkShared>
}

impl Network {
    pub fn new () -> Network {

        let internals = NetworkInternals {
            next_slab_id: 0,
            slabs:     Vec::new(),
            slab_refs: Vec::new(),
            transports: Vec::new(),
            root_index_seed: None
        };

        let shared = NetworkShared {
            internals: Mutex::new(internals)
        };

        let net = Network {
            shared: Arc::new(shared)
        };

        net
    }
    pub fn weak (&self) -> WeakNetwork {
        WeakNetwork {
            shared: Arc::downgrade(&self.shared)
        }
    }
    pub fn add_transport (&self, transport: Box<Transport + Send + Sync> ) {
        let mut internals = self.shared.internals.lock().unwrap();
        transport.bind_network(self);
        internals.transports.push(transport);
    }
    pub fn generate_slab_id(&self) -> u32 {
        let mut internals = self.shared.internals.lock().unwrap();

        let id = internals.next_slab_id;

        internals.next_slab_id += 1;

        id
    }
    pub fn get_slabs(&self) -> Vec<Slab> {
        let mut internals = self.shared.internals.lock().unwrap();
        internals.get_slabs()
    }
    pub fn get_slab (&mut self, slab_id: SlabId ) -> Option<Slab> {
        let mut internals = self.shared.internals.lock().unwrap();
        internals.get_slab(slab_id)
    }
    pub fn assert_slabref_from_presence(&self, presence: &SlabPresence) -> SlabRef {

        {
            let mut internals = self.shared.internals.lock().unwrap();
            match internals.slab_refs.iter().find(|r| r.presence == *presence ) {
                Some(slabref) => {
                    //TODO: should we update the slabref if the address is different?
                    //      or should we find/make a new slabref because its different?
                    return slabref.clone();
                }
                _ =>{}
            }
        }

        let slabref = SlabRef::new_from_presence(presence.clone(), &self);
        self.shared.internals.lock().unwrap().slab_refs.push(slabref.clone());
        return slabref;
    }
    pub fn get_local_transmitter (&self, slab: &Slab) -> Transmitter {
        // We're just going to assume that we have an in-process transmitter, or freak out
        // Should probably do this more intelligently

        let internals = self.shared.internals.lock().unwrap();
        let transport = internals.transports.iter().filter(|x| x.is_local() ).next().unwrap();

        transport.make_transmitter( TransmitterArgs::Local(&slab) ).unwrap()
    }
    pub fn get_remote_transmitter (&self, slab_id: SlabId, address: TransportAddress) -> Transmitter {
        // We're just going to assume that we have an in-process transmitter, or freak out
        // Should probably do this more intelligently

        let internals = self.shared.internals.lock().unwrap();

        match address {
            TransportAddress::UDP(_) => {
                // HACK
                if let Some(transport) = internals.transports.iter().find(|x| !x.is_local() ){
                    return transport.make_transmitter( TransmitterArgs::Remote(&slab_id, address) ).unwrap()
                }
            }
            _ => {}
        }

        panic!("Failed to get remote transmitter");
    }
    pub fn register_slab(&self, slab: &Slab) -> SlabRef {
        println!("# register_slab {:?}", slab );

        // Probably won't use transports in quite the same way in the future

        let slab_ref = SlabRef::new_from_slab( &slab, &self );

        let mut internals = self.shared.internals.lock().unwrap();

        for prev_slab in internals.get_slabs() {
            prev_slab.inject_peer_slabref( slab_ref.clone() );
        }
        for prev_slab_ref in internals.get_slab_refs() {
            slab.inject_peer_slabref( prev_slab_ref.clone() );
        }

        internals.slab_refs.insert( 0, slab_ref.clone() );
        internals.slabs.insert(0, slab.weak() );

        slab_ref
    }

    pub fn get_root_index_seed(&self, slab: &Slab) -> MemoRefHead {

        let mut internals = self.shared.internals.lock().unwrap();

        match internals.root_index_seed {
            Some(ref s) => {
                return s.clone()
            }
            None => {}
        }

        let seed = slab.generate_root_index_seed();
        internals.root_index_seed = Some(seed.clone());
        seed
    }
}

impl NetworkInternals {

    fn get_slab (&mut self, slab_id: SlabId ) -> Option<Slab> {
        if let Some(weak) = self.slabs.iter().find(|s| s.id == slab_id ) {
            if let Some(slab) = weak.upgrade() {
                return Some(slab);
            }
        }

        return None;
    }
    fn get_slabs (&mut self) -> Vec<Slab> {
        // TODO: convert this into a iter generator that automatically expunges missing slabs.
        let mut res: Vec<Slab> = Vec::with_capacity(self.slabs.len());
        //let mut missing : Vec<usize> = Vec::new();

        for slab in self.slabs.iter_mut() {
            match slab.upgrade() {
                Some(s) => {
                    res.push( s );
                },
                None => {
                    // TODO: expunge freed slabs
                }
            }

        }

        res
    }

    fn get_slab_refs (&mut self) -> Vec<SlabRef> {
        // TODO: convert this into a iter generator that automatically expunges missing slabs.
        let mut res: Vec<SlabRef> = Vec::with_capacity(self.slabs.len());
        //let mut missing : Vec<usize> = Vec::new();

        for slab_ref in self.slab_refs.iter() {
            //if slab_ref.is_resident() {
                res.push(slab_ref.clone());
            //}
        }

        res
    }
}

impl fmt::Debug for Network {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        let inner = self.shared.internals.lock().unwrap();

        fmt.debug_struct("Network")
            .field("next_slab_id", &inner.next_slab_id)
            .finish()
    }
}

impl Drop for NetworkInternals {
    fn drop(&mut self) {
        println!("# > Dropping NetworkInternals");
    }
}

impl WeakNetwork {
    pub fn upgrade (&self) -> Option<Network> {
        match self.shared.upgrade() {
            Some(s) => Some( Network { shared: s } ),
            None    => None
        }
    }
}
