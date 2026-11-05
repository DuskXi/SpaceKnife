//! classes.*: the editable file class table (W7 「文件类别」, UI-W7-33; Python
//! `server/classes_handlers.py`).

use std::sync::Arc;

use sk_core::classes::{self, ClassTable};
use sk_core::{pathclass, tr};
use sk_protocol::classes::{ClassFolder, ClassesEvent, ClassesState};
use sk_protocol::methods::{ClassesChanged, ClassesFolder, ClassesGet, ClassesReset, ClassesUpdate};

use crate::app::Server;
use crate::rpc::{RpcError, RpcResult, Router};
use crate::topics::TopicRegistry;

/// The table as the interface shows it: built-in names in the current language (M7.5).
pub fn state() -> ClassesState {
    let table = classes::active();
    ClassesState {
        classes: classes::shown(&table),
        defaults: classes::shown(&ClassTable::builtin()),
        custom: table.custom(),
        folders: pathclass::folders().into_iter().map(|(path, class_id)| ClassFolder { path, class_id }).collect(),
    }
}

/// At start: the user's table and folder choices, if there are any.
pub fn load_saved() {
    classes::load_user();
    pathclass::load_user();
}

/// Tell every session and recolour / refilter every view.
async fn announce(server: &Arc<Server>) -> ClassesState {
    let out = state();
    server.broadcast::<ClassesChanged>(&ClassesEvent { state: out.clone() }, None);
    server.views.reclassify(server).await; // colours, :class: filters, class totals
    out
}

/// Make a table the active one: saved, announced, every view recoloured and refiltered.
pub async fn apply_table(server: &Arc<Server>, table: Arc<ClassTable>) -> RpcResult<ClassesState> {
    let t = table.clone();
    crate::compute::blocking(move || classes::save(&t, &classes::user_file()))
        .await?
        .map_err(|e| RpcError::invalid_request(crate::util::os_message(&e)))?;
    classes::set_active(table);
    Ok(announce(server).await)
}

/// The folder choices changed: saved, announced, every view recoloured and refiltered.
pub async fn apply_folders(server: &Arc<Server>) -> RpcResult<ClassesState> {
    crate::compute::blocking(|| pathclass::save(&pathclass::user_file()))
        .await?
        .map_err(|e| RpcError::invalid_request(crate::util::os_message(&e)))?;
    Ok(announce(server).await)
}

pub fn install(r: &mut Router, _t: &mut TopicRegistry) {
    r.add::<ClassesGet, _, _>(|_, _| async move { Ok(state()) });

    r.add::<ClassesUpdate, _, _>(|call, p| async move {
        let table = classes::build(&p.classes).map_err(RpcError::invalid_params)?;
        apply_table(&call.server, Arc::new(table)).await
    });

    r.add::<ClassesFolder, _, _>(|call, p| async move {
        if let Some(cid) = p.class_id.as_deref()
            && !cid.is_empty()
            && classes::active().by_id(cid).is_none()
        {
            return Err(RpcError::invalid_params(tr!("没有 id 为「{cid}」的类别", cid = cid)));
        }
        pathclass::set_folder(&p.path, p.class_id.as_deref());
        apply_folders(&call.server).await
    });

    // the built-in classes, and no folder choices (every folder recognised by the rules again)
    r.add::<ClassesReset, _, _>(|call, _| async move {
        if !pathclass::folders().is_empty() {
            pathclass::replace_folders(Vec::new());
            crate::compute::blocking(|| pathclass::save(&pathclass::user_file()))
                .await?
                .map_err(|e| RpcError::invalid_request(crate::util::os_message(&e)))?;
        }
        apply_table(&call.server, ClassTable::builtin()).await
    });
}
