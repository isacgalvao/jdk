//! Elevation probe for `jdk doctor` — informative ONLY, never a command
//! gate: jdk writes per-user state (HKCU, junction) and needs no admin.
//! Win32 sequence: the canonical Microsoft admin check reimplemented over
//! windows-sys.

use windows_sys::Win32::Foundation::FALSE;
use windows_sys::Win32::Security::{
    AllocateAndInitializeSid, CheckTokenMembership, FreeSid, PSID, SID_IDENTIFIER_AUTHORITY,
};

const SECURITY_NT_AUTHORITY: SID_IDENTIFIER_AUTHORITY = SID_IDENTIFIER_AUTHORITY {
    Value: [0, 0, 0, 0, 0, 5],
};
const SECURITY_BUILTIN_DOMAIN_RID: u32 = 0x20;
const DOMAIN_ALIAS_RID_ADMINS: u32 = 0x220;

/// Whether this process runs elevated (member of BUILTIN\Administrators,
/// SID S-1-5-32-544). Any API failure reads as "not elevated".
pub fn is_admin() -> bool {
    // FreeSid on every path, error or not (RAII).
    struct Sid(PSID);
    impl Drop for Sid {
        fn drop(&mut self) {
            unsafe { FreeSid(self.0) };
        }
    }

    unsafe {
        let mut psid: PSID = std::ptr::null_mut();
        let allocated = AllocateAndInitializeSid(
            &SECURITY_NT_AUTHORITY,
            2,
            SECURITY_BUILTIN_DOMAIN_RID,
            DOMAIN_ALIAS_RID_ADMINS,
            0,
            0,
            0,
            0,
            0,
            0,
            &mut psid,
        );
        if allocated == FALSE {
            return false;
        }
        let sid = Sid(psid);

        // Token NULL = the current thread's effective token (intentional —
        // this is how CheckTokenMembership consults the running process).
        let mut member = FALSE;
        let checked = CheckTokenMembership(std::ptr::null_mut(), sid.0, &mut member);
        checked != FALSE && member != FALSE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_without_crashing_and_is_deterministic() {
        // The answer depends on how the test runner was launched; what the
        // test can pin is that the probe is stable and leak-free to call.
        assert_eq!(is_admin(), is_admin());
    }
}
