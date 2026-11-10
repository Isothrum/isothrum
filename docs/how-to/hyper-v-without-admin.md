# Use Hyper-V without administrator rights

Hyper-V accepts management commands only from an administrator or a member of the local
**Hyper-V Administrators** group, and Vagrant's Hyper-V provider refuses to run otherwise.

Before an operation that acts on a Hyper-V run (`up`, `down`, `park`, `resume`, ...; `status`
excepted), Isothrum checks the account's groups:

- **Elevated, or a member of Hyper-V Administrators:** the tools run directly. No prompt.
- **Otherwise:** Isothrum starts one elevated helper for the operation, which shows one UAC
  prompt. It waits up to **300 seconds** for the prompt to be approved; declined or unanswered,
  the operation fails with `elevation_failed` (as it does when the helper stops early). Every command of the operation then goes through
  that helper, so there is one prompt per operation, not one per command.

The prompt needs someone at the desktop. For unattended use (CI, scheduled jobs, a service) the
account must be in the group or already elevated.

## Join Hyper-V Administrators

In an elevated PowerShell (as an administrator, once):

```powershell
Add-LocalGroupMember -SID S-1-5-32-578 -Member "$env:USERDOMAIN\$env:USERNAME"
```

`S-1-5-32-578` is the group's well-known SID, which works whatever the Windows display language.
Replace the member with the account that runs `isothrum` if it isn't yours.

Then **sign out of Windows and back in**: a new group membership applies from the next logon.

Check it in a new, non-elevated PowerShell:

```powershell
whoami /groups | Select-String S-1-5-32-578
```

A line with `S-1-5-32-578` means the next `isothrum up --provider hyperv` runs without a prompt.

On a managed computer, local group membership may be set by policy; ask the administrators of the
domain to add the account.

## Detect it from your own program

`isothrum-machine`'s `providers::detect(tools, vagrant_installed, may_manage_hyperv)` takes your
own answer to "may this account manage Hyper-V" and, given `Some(false)`, reports Hyper-V with the
reason and the way out above.
