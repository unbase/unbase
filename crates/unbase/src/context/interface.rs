use crate::{
    error::{
        RetrieveError,
        WriteError,
    },
    index::IndexFixed,
    memorefhead::MemoRefHead,
    slab::{
        EdgeLink,
        EdgeSet,
        MemoBody,
        MemoId,
        RelationSet,
        RelationSlotId,
    },
    subject::{
        SubjectType,
        SubjectId,
    },
    subjecthandle::SubjectHandle,
};
use super::Context;
use timer::Delay;

use std::{
    collections::HashMap,
    fmt,
    sync::Arc,
    time::{Instant,Duration}
};

/// User interface functions - Programmer API for `Context`
impl Context {
    /// Retrive a Subject from the root index by ID
    pub async fn get_subject_by_id(&self, subject_id: SubjectId) -> Result<Option<SubjectHandle>, RetrieveError> {

        let root_index = self.root_index(Duration::from_secs(5)).await?;

        match root_index.get(&self, subject_id.id).await? {
            Some(s) => {
                let sh = SubjectHandle{
                    id: subject_id,
                    subject: s,
                    context: self.clone()
                };

                Ok(Some(sh))
            },
            None => Ok(None)
        }
    }

    pub fn concise_contents (&self) -> Vec<String> {
        self.stash.concise_contents()
    }

    // Magically transport subject heads into another context in the same process.
    // This is a temporary hack for testing purposes until such time as proper context exchange is enabled
    // QUESTION: should context exchanges be happening constantly, but often ignored? or requested? Probably the former,
    //           sent based on an interval and/or compaction ( which would also likely be based on an interval and/or present context size)
    #[tracing::instrument]
    pub async fn hack_send_context(&self, other: &Context) -> Result<usize,WriteError> {
        self.compact().await?;

        let from_slabref = other.slab.agent.localize_slabref(&self.slab.my_ref);

        let mut memoref_count = 0;

        for head in self.stash.iter() {
            memoref_count += head.len();

            let apply_head = other.slab.agent.localize_memorefhead(&head, &from_slabref, false);
            other.apply_head(&apply_head).await?;
        }

        Ok(memoref_count)
    }
    pub async fn get_relevant_subject_head(&self, subject_id: SubjectId) -> Result<MemoRefHead, RetrieveError> {
        match subject_id {
            SubjectId{ stype: SubjectType::IndexNode,.. } => {
                Ok(self.stash.get_head(subject_id).clone())
            },
            SubjectId{ stype: SubjectType::Record, .. } => {
                // TODO: figure out a way to noop here in the case that the SubjectHead in question
                //       was pulled against a sufficiently identical context stash state.
                //       Perhaps stash edit increment? how can we get this to be really granular?

                let root_index = self.root_index(Duration::from_secs(5)).await?;

                match root_index.get_head(&self, subject_id.id).await? {
                    Some(mrh) => Ok(mrh),
                    None      => Ok(MemoRefHead::Null)
                }
            }
        }
    }
    pub fn try_root_index (&self) -> Result<Arc<IndexFixed>,RetrieveError> {
        {
           let rg = self.root_index.read().unwrap();
           if let Some( ref arcindex ) = *rg {
               return Ok( arcindex.clone() )
           }
        };
        
        let seed = self.slab.net.get_root_index_seed(&self.slab);
        if seed.is_some() {
            let index = IndexFixed::new_from_memorefhead(&self, 5, seed);
            let arcindex = Arc::new(index);
            *self.root_index.write().unwrap() = Some(arcindex.clone());

            Ok(arcindex)
        }else{
            Err(RetrieveError::IndexNotInitialized)
        }

    }
    pub async fn root_index (&self, wait: Duration) -> Result<Arc<IndexFixed>, RetrieveError> {
        let start = Instant::now();

        loop {
            if start.elapsed() > wait{
                return Err(RetrieveError::NotFoundByDeadline)
            }

            if let Ok(ri) = self.try_root_index() {
                return Ok(ri);
            };

            Delay::new(Duration::from_millis(50)).await;
        }
    }
    pub fn get_resident_subject_head(&self, subject_id: SubjectId) -> MemoRefHead {
        self.stash.get_head(subject_id).clone()
    }
    pub fn get_resident_subject_head_memo_ids(&self, subject_id: SubjectId) -> Vec<MemoId> {
        self.get_resident_subject_head(subject_id).memo_ids()
    }
    pub fn cmp(&self, other: &Self) -> bool {
        // stable way:
        &*(self.0) as *const _ != &*(other.0) as *const _

        // unstable way:
        // Arc::ptr_eq(&self.inner,&other.inner)
    }

    pub async fn add_test_subject(&self, subject_id: SubjectId, relations: Vec<MemoRefHead>) -> MemoRefHead {

        let mut edgeset = EdgeSet::empty();

        for (slot_id, mrh) in relations.iter().enumerate() {
            if let &MemoRefHead::Subject{..} = mrh {
                edgeset.insert(slot_id as RelationSlotId, mrh.clone())
            }
        }

        let head = self.slab.new_memo(
            Some(subject_id),
            MemoRefHead::Null,
            MemoBody::FullyMaterialized { v: HashMap::new(), r: RelationSet::empty(), e: edgeset, t: subject_id.stype }
        ).to_head();

        self.apply_head(&head).await.expect("apply head")
    }

    /// Attempt to compress the present query context.
    /// We do this by issuing Relation memos for any subject heads which reference other subject heads presently in the query context.
    /// Then we can remove the now-referenced subject heads, and repeat the process in a topological fashion, confident that these
    /// referenced subject heads will necessarily be included in subsequent projection as a result.
    pub async fn compact(&self) -> Result<(), WriteError>  {
        let before = self.stash.concise_contents();

        //TODO: implement topological MRH iterator for stash
        //      non-topological iteration will yield sub-optimal compaction

        // iterate all heads in the stash
        for parent_mrh in self.stash.iter() {
            let mut updated_edges = EdgeSet::empty();

            for edgelink in parent_mrh.project_occupied_edges(&self.slab).await? {
                if let EdgeLink::Occupied{slot_id,head:edge_mrh} = edgelink {

                    if let Some(subject_id) = edge_mrh.subject_id(){
                        if let stash_mrh @ MemoRefHead::Subject{..} = self.stash.get_head(subject_id) {
                            // looking for cases where the stash is fresher than the edge
                            if stash_mrh.descends_or_contains(&edge_mrh, &self.slab).await?{
                                updated_edges.insert( slot_id, stash_mrh );
                            }
                        }
                    }
                }
            }

            if updated_edges.len() > 0 {
                // TODO: When should this be materialized?
                let memobody = MemoBody::Edge(updated_edges);
                let subject_id = parent_mrh.subject_id().unwrap();

                let head = self.slab.new_memo(
                    Some(subject_id),
                    MemoRefHead::Null,
                    memobody.clone()
                ).to_head();
                
                self.apply_head(&head).await?;
            }
        }

        println!("COMPACT Before: {:?}, After: {:?}", before, self.stash.concise_contents() );
        Ok(())
    }

    pub fn is_fully_materialized(&self) -> bool {
        unimplemented!();
        // for subject_head in self.manager.subject_head_iter() {
        //     if !subject_head.head.is_fully_materialized(&self.slab) {
        //         return false;
        //     }
        // }
        // return true;

    }
}

impl fmt::Debug for Context {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {

        fmt.debug_struct("ContextShared")
            .field("subject_heads", &self.stash.subject_ids() )
            .finish()
    }
}