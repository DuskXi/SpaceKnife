//! update.*: the update checks, their schedule and the one-click download (`updates.v1`; Python
//! `server/update_handlers.py`, the service is [`crate::updates`]).

use sk_protocol::methods::{
    UpdateCancel, UpdateCheck, UpdateDismiss, UpdateDownloadMethod, UpdateMute,
    UpdateReleasesMethod, UpdateStatusMethod,
};

use crate::rpc::Router;
use crate::topics::TopicRegistry;

pub fn install(r: &mut Router, _t: &mut TopicRegistry) {
    r.add::<UpdateStatusMethod, _, _>(|call, _| async move { Ok(call.server.updates.status()) });
    r.add::<UpdateCheck, _, _>(|call, _| async move { Ok(call.server.updates.check().await) });
    r.add::<UpdateDismiss, _, _>(|call, p| async move {
        Ok(call.server.updates.dismiss(&p.version).await)
    });
    r.add::<UpdateMute, _, _>(|call, p| async move {
        Ok(call.server.updates.mute(&p.series, p.muted).await)
    });
    r.add::<UpdateDownloadMethod, _, _>(|call, p| async move {
        call.server.updates.download(&p.version).await
    });
    r.add::<UpdateReleasesMethod, _, _>(
        |call, _| async move { Ok(call.server.updates.releases()) },
    );
    r.add::<UpdateCancel, _, _>(|call, _| async move { Ok(call.server.updates.cancel()) });
}
