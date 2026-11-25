//! What a start does before its `up`: a record on another provider cleared, and VirtualBox's cut-off saved state discarded.

use super::{Provider, READ_LIMIT, RECONCILE_LIMIT, Result, ToolCx, Vagrant, Verb, WarningCode, call, dot, tool_error, vboxmanage};

impl Vagrant {
    /// The preamble of `up` and `resume`: a record on another provider cleared, a cut-off saved
    /// state discarded.
    pub(super) async fn preamble(&self, cx: &ToolCx<'_>, provider: Provider) -> Result<()> {
        self.reconcile_provider(cx, provider).await?;
        self.discard_aborted_saved(cx, provider).await
    }

    /// A run recorded under another provider than `provider` (it last ran on VirtualBox, now on
    /// libvirt) is destroyed, given [`RECONCILE_LIMIT`] and best effort, and its record dropped.
    pub(super) async fn reconcile_provider(&self, cx: &ToolCx<'_>, provider: Provider) -> Result<()> {
        let dot = dot(cx);
        if !dot.recorded()?.iter().any(|r| r.provider != provider.id()) {
            return Ok(());
        }
        (cx.warn)(WarningCode::OtherProvider, "this run last ran on another provider: clearing that run first");
        if let Err(e) = self.act(cx, Verb::Destroy, &[], RECONCILE_LIMIT).await {
            (cx.warn)(WarningCode::CleanupFailed, &format!("the run on the other provider couldn't be destroyed: {e}"));
        }
        dot.reset()
    }

    /// A VirtualBox VM of the run whose saved state was cut off (a suspend killed midway, a host
    /// crash) has it discarded, so it boots fresh from its disk: VirtualBox refuses every change to
    /// such a VM. A VM whose state can't be read is left as it is (`up` says what is wrong).
    pub(super) async fn discard_aborted_saved(&self, cx: &ToolCx<'_>, provider: Provider) -> Result<()> {
        if provider != Provider::Virtualbox {
            return Ok(());
        }
        let call = call(cx, READ_LIMIT);
        for r in dot(cx).recorded()?.into_iter().filter(|r| r.provider == provider.id()) {
            match vboxmanage::vm_state(&call, &r.id).await {
                Ok(vboxmanage::VmState::AbortedSaved) => {
                    (cx.warn)(
                        WarningCode::SavedStateDiscarded,
                        &format!("{}: its saved state was cut off; it boots fresh", r.machine),
                    );
                    vboxmanage::discard_state(&call, &r.id).await.map_err(|e| tool_error("discard-saved", e))?;
                }
                Ok(vboxmanage::VmState::Other(_)) => {}
                Err(e) => tracing::debug!(vm = %r.machine, error = %e, "the VM's state couldn't be read: its saved state, if any, is left as it is"),
            }
        }
        Ok(())
    }
}
