use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

/// A connection, numbered in the order they were made.
#[derive(Debug)]
struct Conn(usize);

/// A pool whose connections are numbered from 1, and the count made so far.
fn numbered() -> (Pool<Conn>, Arc<AtomicUsize>) {
    let made = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&made);
    let pool = Pool::with(move |_home| {
        let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move { Ok(Conn(n)) })
    });
    (pool, made)
}

fn broken() -> ClientError {
    ClientError::Io(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
}

#[tokio::test]
async fn one_connection_per_home_is_kept() {
    let (pool, made) = numbered();
    assert_eq!(pool.client(Path::new("/a")).await.unwrap().0, 1);
    assert_eq!(pool.client(Path::new("/a")).await.unwrap().0, 1);
    assert_eq!(pool.client(Path::new("/b")).await.unwrap().0, 2);
    pool.forget(Path::new("/a")).await;
    assert_eq!(pool.client(Path::new("/a")).await.unwrap().0, 3);
    pool.retain(|home| home == Path::new("/a")).await;
    assert_eq!(pool.client(Path::new("/b")).await.unwrap().0, 4);
    assert_eq!(made.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn a_broken_connection_is_made_again_once() {
    let (pool, _) = numbered();
    let got = pool
        .call(Path::new("/a"), |c| async move { if c.0 == 1 { Err(broken()) } else { Ok(c.0) } })
        .await
        .unwrap();
    assert_eq!(got, 2);
    // The new connection is the one kept.
    assert_eq!(pool.client(Path::new("/a")).await.unwrap().0, 2);
}

#[tokio::test]
async fn a_second_break_is_the_callers_and_forgotten() {
    let (pool, made) = numbered();
    let err = pool.call(Path::new("/a"), |_| async { Err::<(), _>(broken()) }).await.unwrap_err();
    assert!(err.is_broken(), "{err:?}");
    assert_eq!(made.load(Ordering::SeqCst), 2, "tried once more, not again");
    assert_eq!(pool.client(Path::new("/a")).await.unwrap().0, 3, "the broken one isn't kept");
}

#[tokio::test]
async fn an_answer_of_the_service_is_not_tried_again() {
    let (pool, made) = numbered();
    let refused = ClientError::Version {
        client: "1".into(),
        service: "2".into(),
    };
    assert!(!refused.is_broken());
    let err = pool
        .call(Path::new("/a"), |_| async {
            Err::<(), _>(ClientError::Version {
                client: "1".into(),
                service: "2".into(),
            })
        })
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::Version { .. }), "{err:?}");
    assert_eq!(made.load(Ordering::SeqCst), 1);
    assert_eq!(pool.client(Path::new("/a")).await.unwrap().0, 1, "kept");
}

#[tokio::test]
async fn a_connection_that_cant_be_made_is_the_callers() {
    let pool: Pool<Conn> = Pool::with(|_| Box::pin(async { Err(ClientError::Unavailable("none".into())) }));
    let err = pool.call(Path::new("/a"), |c| async move { Ok(c.0) }).await.unwrap_err();
    assert!(matches!(err, ClientError::Unavailable(_)), "{err:?}");
}
