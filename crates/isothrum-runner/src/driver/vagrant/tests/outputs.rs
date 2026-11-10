//! What Vagrant, its providers and `VBoxManage` print in the cases the fake Vagrant models: their
//! texts as seen on real hosts.

/// What vagrant-vmware-esxi prints when it is asked to halt a VM that is already off.
pub(super) const ESXI_ALREADY_OFF: &str = "There was an error talking to ESXi.
  Unable to shutdown the VM:
    (vim.fault.InvalidPowerState) {
   faultCause = (vmodl.MethodFault) null,
   faultMessage = <unset>,
   requestedState = \"poweredOn\",
   existingState = \"poweredOff\"
   msg = \"Received SOAP response fault from [<TCP '127.0.0.1 : 8307'>, /sdk>]: shutdownGuest
The attempted operation cannot be performed in the current state (Powered off).\"
}";

/// What `VBoxManage` prints when VirtualBox's service doesn't answer.
pub(super) const VBOX_WEDGED: &str = "VBoxManage: error: Failed to create the VirtualBox object! (NS_ERROR_FAILURE)";

/// What `vagrant status --machine-readable` prints on Hyper-V when another Vagrant process
/// holds `vm`'s lock (seen on a Windows host with Hyper-V, while another process running
/// `vagrant global-status --prune` read the run's VMs).
pub(super) fn locked_status(vm: &str) -> Vec<String> {
    let said = format!(
        "An action 'read_state' was attempted on the machine '{vm}'%!(VAGRANT_COMMA)\\nbut another process is already executing \
         an action on the machine.\\nVagrant locks each machine for access by only one process at a time."
    );
    vec![
        "1794307172,isoloom-controller,action,read_state,start".to_owned(),
        "1794307174,isoloom-controller,action,read_state,end".to_owned(),
        format!("1794307174,,ui,error,{said}"),
        format!("1794307174,,error-exit,Vagrant::Errors::MachineActionLockedError,{said}"),
    ]
}

/// What vagrant-libvirt's fog prints on standard error at each of Vagrant's calls, once per
/// machine it reads.
pub(super) const FOG_WARNING: &str = "[fog][WARNING] Unrecognized arguments: libvirt_ip_command";

/// What `vagrant status --machine-readable` prints when another Vagrant process holds `vm`'s
/// machine index lock (Vagrant 2.4.9 on libvirt, a `vagrant status` at the same time): the error
/// on standard output, fog's warnings alone on standard error.
pub(super) fn index_locked_status(vm: &str) -> Vec<String> {
    let said = format!(
        "Vagrant can't use the requested machine because it is locked! This\\nmeans that another Vagrant process is \
         currently reading or modifying\\nthe machine. Please wait for that Vagrant process to end and try\\nagain. Details \
         about the machine are shown below:\\n\\nName: {vm}\\nProvider: libvirt"
    );
    vec![
        format!("1794342958,,ui,error,{said}"),
        format!("1794342958,,error-exit,Vagrant::Errors::MachineLocked,{said}"),
    ]
}

/// What `vagrant halt <vm>` prints when another Vagrant process holds `vm`'s lock.
pub(super) fn locked_halt(vm: &str) -> String {
    format!(
        "An action 'read_state' was attempted on the machine '{vm}',\nbut another process is already executing an action on the machine.\n\
         Vagrant locks each machine for access by only one process at a time."
    )
}

/// What `VBoxManage unregistervm --delete` prints for a VM another process has a session on.
pub(super) const VBOX_LOCKED: &str = "VBoxManage: error: Cannot unregister the machine 'web-vm - web' while it is locked";
