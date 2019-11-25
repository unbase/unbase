extern crate unbase;
use unbase::subject::*;
use unbase::context::ContextRef;
use unbase::index::IndexFixed;
use std::collections::HashMap;
use futures_await_test::async_test;

#[async_test]
async fn index_construction() {

    let net = unbase::Network::create_new_system();
    let simulator = unbase::network::transport::Simulator::new();
    net.add_transport( Box::new(simulator.clone()) );

    let slab_a = unbase::Slab::new(&net);
    let context_a = slab_a.create_context();

    // Create a new fixed tier index (fancier indexes not necessary for the proof of concept)

    let index = IndexFixed::new(&ContextRef::Strong(context_a.clone()), 5).await;

    assert_eq!( context_a.is_fully_materialized(), true );

    // First lets do a single index test
    let i = 1234;
    let mut vals = HashMap::new();
    vals.insert("record number".to_string(), i.to_string());

    let record = Subject::new(&context_a, vals, false).await.unwrap();
    index.insert(i, &record).await;

    assert_eq!( index.get(1234).await.unwrap().get_value("record number").await.unwrap(), "1234");


    // Ok, now lets torture it a little
    for i in 0..10 {
        let mut vals = HashMap::new();
        vals.insert("record number".to_string(), i.to_string());

        let record = Subject::new(&context_a, vals, false).await.unwrap();
        index.insert(i, &record).await;
    }

    for i in 0..10 {
        assert_eq!( index.get(i).await.unwrap().get_value("record number").await.unwrap(), i.to_string() );
    }

    //assert_eq!( context_a.is_fully_materialized(), false );
    //context_a.fully_materialize();
}
