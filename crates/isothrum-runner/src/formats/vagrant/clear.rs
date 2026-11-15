//! Clearing what a run made, for a start to begin from nothing, and removing a machine at its
//! hypervisor by its recorded id.

use super::screen::LIBVIRT_URI;
use super::{CALL_LIMIT, CliError, Error, Provider, READ_LIMIT, Record, Result, ToolCx, Vagrant, Verb, WarningCode, call, dot, ours, tool_error, vboxmanage};

impl Vagrant {
    /// Removes what the run made, for a start to begin from nothing: `destroy -f` (best effort),
    /// each VM the dotfile still records by its id, on VirtualBox the VMs and folders whose name
    /// carries the run's key, then the dotfile.
    pub(super) async fn clear(&self, cx: &ToolCx<'_>) -> Result<()> {
        if let Err(e) = self.act(cx, Verb::Destroy, &[], CALL_LIMIT).await {
            tracing::debug!(error = %e, "the clearing destroy failed: the VMs are removed by id");
        }
        let dot = dot(cx);
        for r in dot.recorded()? {
            self.remove_at_hypervisor(cx, &r).await?;
        }
        if cx.provider == Some(Provider::Virtualbox) {
            self.virtualbox_leftovers(cx).await;
        }
        dot.reset()
    }

    /// VirtualBox's VMs and VM folders whose name carries the run's key, removed: what a crashed
    /// import leaves, unknown to Vagrant, and what makes the next import fail with
    /// `VERR_ALREADY_EXISTS`. Each failure is a `cleanup_failed` warning: the retried `up` says the
    /// rest.
    pub(super) async fn virtualbox_leftovers(&self, cx: &ToolCx<'_>) {
        let call = super::read(cx, READ_LIMIT);
        match vboxmanage::vms(&call).await {
            Ok(vms) => {
                for (name, uuid) in vms.into_iter().filter(|(n, _)| ours(n, cx.key)) {
                    drop(vboxmanage::discard_state(&call, &uuid).await);
                    drop(vboxmanage::power_off(&call, &uuid).await);
                    match vboxmanage::unregister_delete(&call, &uuid).await {
                        Ok(())
                        | Err(CliError::Failed {
                            failure: vboxmanage::Failure::NotFound,
                            ..
                        }) => {
                            (cx.warn)(WarningCode::OrphanRemoved, &format!("removed a VM an earlier run left ({name})"));
                        }
                        Err(e) => (cx.warn)(
                            WarningCode::CleanupFailed,
                            &format!("a VM an earlier run left ({name}) couldn't be removed: {e}"),
                        ),
                    }
                }
            }
            Err(e) => (cx.warn)(
                WarningCode::CleanupFailed,
                &format!("VirtualBox's VMs couldn't be listed: none is removed: {e}"),
            ),
        }
        let Ok(base) = vboxmanage::machine_folder(&call).await else { return };
        let Ok(entries) = std::fs::read_dir(&base) else { return };
        for entry in entries.flatten() {
            let is_folder = entry.file_type().is_ok_and(|t| t.is_dir() || t.is_symlink());
            if is_folder && ours(&entry.file_name().to_string_lossy(), cx.key) {
                let what = format!("a VM folder an earlier run left ({})", entry.path().display());
                match crate::store::remove_path(&entry.path()) {
                    Ok(()) => (cx.warn)(WarningCode::OrphanRemoved, &format!("removed {what}")),
                    Err(e) => (cx.warn)(WarningCode::CleanupFailed, &format!("{what} couldn't be removed: {e}")),
                }
            }
        }
    }

    /// Removes `record`'s VM at its hypervisor by its id, Vagrant not involved (a machine the
    /// Vagrantfile no longer defines, a record Vagrant can't act on); a VM already gone is fine.
    /// Only VirtualBox has an adapter: any other provider is refused, named.
    pub(super) async fn remove_at_hypervisor(&self, cx: &ToolCx<'_>, record: &Record) -> Result<()> {
        let call = call(cx, CALL_LIMIT);
        let removed = match crate::encoding::from_wire_name::<Provider>(&record.provider) {
            Some(Provider::Virtualbox) => {
                drop(vboxmanage::power_off(&call, &record.id).await);
                match vboxmanage::unregister_delete(&call, &record.id).await {
                    Ok(())
                    | Err(CliError::Failed {
                        failure: vboxmanage::Failure::NotFound,
                        ..
                    }) => Ok(()),
                    Err(e) => Err(tool_error("remove", e)),
                }
            }
            Some(Provider::Libvirt) => {
                let uri = cx.var(crate::host_env::LIBVIRT_DEFAULT_URI).is_none().then_some(LIBVIRT_URI);
                crate::cli::virsh::remove(&call, uri, &record.id).await.map_err(|e| tool_error("remove", e))
            }
            Some(Provider::Parallels) => crate::cli::prlctl::remove(&call, &record.id).await.map_err(|e| tool_error("remove", e)),
            Some(Provider::Utm) => crate::cli::utmctl::remove(&call, &record.id).await.map_err(|e| tool_error("remove", e)),
            _ => Err(Error::Unsupported {
                what: format!(
                    "removing VM `{}` of machine `{}` on `{}` by its id once Vagrant can't (VirtualBox, libvirt, Parallels and UTM VMs are)",
                    record.id, record.machine, record.provider
                ),
            }),
        };
        removed?;
        dot(cx).forget(&record.machine, &record.provider)
    }
}
