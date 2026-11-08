//! path.forms, basket.* and ops.*: the delete basket and file operations (W5; Python
//! `server/ops_handlers.py`). The state is [`crate::ops_service::OpsService`].

use sk_protocol::EmptyResult;
use sk_protocol::methods::{
    BasketAdd, BasketClear, BasketExecute, BasketList, BasketRemove, OpsCancel, OpsUndo,
    PathFormsMethod,
};

use crate::rpc::Router;
use crate::topics::TopicRegistry;

pub fn install(r: &mut Router, _t: &mut TopicRegistry) {
    r.add::<PathFormsMethod, _, _>(|call, p| async move {
        call.server
            .ops
            .path_forms(&call.server, &p.scan_id, p.node)
            .await
    });
    r.add::<BasketList, _, _>(|call, _| async move { call.server.ops.state(&call.server).await });
    r.add::<BasketAdd, _, _>(|call, p| async move {
        call.server.ops.add(&call.server, &call.session, p).await
    });
    r.add::<BasketRemove, _, _>(|call, p| async move {
        call.server.ops.remove(&call.server, &p.ids).await
    });
    r.add::<BasketClear, _, _>(|call, _| async move { call.server.ops.clear(&call.server).await });
    r.add::<BasketExecute, _, _>(|call, p| async move {
        call.server.ops.execute(&call.server, &call.session, &p)
    });
    r.add::<OpsCancel, _, _>(|call, p| async move {
        call.server.ops.cancel(&p.task_id)?;
        Ok(EmptyResult {})
    });
    r.add::<OpsUndo, _, _>(|call, p| async move {
        call.server.ops.undo(&call.server, &p.task_id).await?;
        Ok(EmptyResult {})
    });
}
