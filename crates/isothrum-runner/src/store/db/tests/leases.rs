//! The home's leases and the host ports reserved under them: one live holder at a time, taken over
//! from a dead or expired one.

use super::*;

/// An owner whose process is gone, and this process.
#[cfg(unix)]
fn dead_and_me() -> (Owner, Owner) {
    let mut dead = Owner::this_process();
    dead.pid = owner::tests::dead_pid();
    (dead, Owner::this_process())
}

/// A store with two runs, `one` and `two`, each with an `up` journaled.
fn two_runs_up() -> (tempfile::TempDir, Store, (RunName, OperationRow), (RunName, OperationRow)) {
    let (home, store) = store();
    let a = run_with_operation(&store, "one", Op::Up);
    let b = run_with_operation(&store, "two", Op::Up);
    (home, store, (name("one"), a), (name("two"), b))
}

#[test]
fn a_lease_is_refused_while_its_holder_lives_and_taken_over_from_a_dead_one() {
    let (_home, store) = store();
    let (me, other) = (Owner::this_process(), Owner::this_process());
    let ttl = Duration::from_secs(60);
    store
        .write(|tx| {
            assert_eq!(tx.acquire_lease("run:r", &me, ttl)?, LeaseOutcome::Acquired { took_over: None });
            assert_eq!(
                tx.acquire_lease("run:r", &me, ttl)?,
                LeaseOutcome::Acquired { took_over: None },
                "its own, renewed"
            );
            assert!(matches!(tx.acquire_lease("run:r", &other, ttl)?, LeaseOutcome::Held(l) if l.owner == me));
            assert!(tx.renew_lease("run:r", &me, ttl)?);
            assert!(!tx.renew_lease("run:r", &other, ttl)?);
            tx.release_lease("run:r", &other)?;
            assert!(tx.lease("run:r")?.is_some(), "another's release takes nothing");
            tx.release_lease("run:r", &me)?;
            assert_eq!(tx.lease("run:r")?, None);
            Ok(())
        })
        .unwrap();
}

#[cfg(unix)]
#[test]
fn a_lease_whose_holder_died_is_taken_over() {
    let (_home, store) = store();
    let (dead, me) = dead_and_me();
    store
        .write(|tx| {
            tx.acquire_lease("run:r", &dead, Duration::from_secs(3600))?;
            match tx.acquire_lease("run:r", &me, Duration::from_secs(60))? {
                LeaseOutcome::Acquired { took_over: Some(left) } => assert_eq!(left.owner, dead),
                other => panic!("taken over: {other:?}"),
            }
            assert_eq!(tx.lease("run:r")?.unwrap().owner, me);
            Ok(())
        })
        .unwrap();
}

#[test]
fn an_expired_lease_is_taken_over() {
    let (_home, store) = store();
    let (first, second) = (Owner::this_process(), Owner::this_process());
    store
        .write(|tx| {
            tx.acquire_lease("home:slots", &first, Duration::ZERO)?;
            assert!(matches!(
                tx.acquire_lease("home:slots", &second, Duration::from_secs(60))?,
                LeaseOutcome::Acquired { took_over: Some(_) }
            ));
            Ok(())
        })
        .unwrap();
}

/// The port both tests reserve.
const PORT: crate::port::HostPort = crate::port::HostPort::new(8080);

#[test]
fn a_port_is_reserved_by_one_live_operation_and_freed_with_its_lease() {
    let (_home, store, (one, a), (two, b)) = two_runs_up();
    let (me, other) = (Owner::this_process(), Owner::this_process());
    let ttl = Duration::from_secs(60);
    let (as_one, as_two) = (
        Reserver::new(&one, Some(a.id), "run:one", &me),
        Reserver::new(&two, Some(b.id), "run:two", &other),
    );
    store
        .write(|tx| {
            tx.acquire_lease("run:one", &me, ttl)?;
            tx.acquire_lease("run:two", &other, ttl)?;
            assert!(tx.reserve_port(&as_one, PORT, Protocol::Tcp)?);
            assert!(tx.reserve_port(&as_one, PORT, Protocol::Tcp)?, "its own again");
            assert!(!tx.reserve_port(&as_two, PORT, Protocol::Tcp)?);
            assert!(tx.reserve_port(&as_two, PORT, Protocol::Udp)?, "another protocol");
            assert_eq!(tx.port_holder(PORT, Protocol::Tcp)?.map(|h| h.run), Some(one.clone()));
            assert_eq!(tx.port_reservations()?.len(), 2);
            tx.release_lease("run:one", &me)?;
            assert!(tx.reserve_port(&as_two, PORT, Protocol::Tcp)?, "freed with its lease");
            tx.release_ports(b.id)?;
            assert_eq!(tx.port_reservations()?, []);
            Ok(())
        })
        .unwrap();
}

#[cfg(unix)]
#[test]
fn a_port_a_dead_process_reserved_is_taken_over() {
    let (_home, store, (one, a), (two, b)) = two_runs_up();
    let (dead, me) = dead_and_me();
    store
        .write(|tx| {
            tx.acquire_lease("run:one", &dead, Duration::from_secs(3600))?;
            assert!(tx.reserve_port(&Reserver::new(&one, Some(a.id), "run:one", &dead), PORT, Protocol::Tcp)?);
            tx.acquire_lease("run:two", &me, Duration::from_secs(60))?;
            assert!(tx.reserve_port(&Reserver::new(&two, Some(b.id), "run:two", &me), PORT, Protocol::Tcp)?);
            Ok(())
        })
        .unwrap();
}
