//! Мирроринг `crates/http-ng-rt-tokio/tests/tokio_socket_opts_tests.rs` для
//! smol-бэкенда. Существует, потому что именно в `connect()` был дефект
//! брифа/скелетона: `async_net::TcpStream::connect(addr)` не принимает опций
//! вовсе, так что `reuse_address`, `send_buffer_size`, `recv_buffer_size` и
//! `local_address` молча терялись, а `nodelay`/`keepalive` применялись уже
//! ПОСЛЕ `connect()`. Каждый тест здесь читает опцию обратно с реально
//! соединённого сокета, а не полагается на факт, что `connect()` вернул `Ok`.
//!
//! Те же два design-решения, что и в `tokio_socket_opts_tests.rs`, сохранены
//! намеренно:
//!
//! 1. Буферные размеры сравниваются как "два РАЗНЫХ явных запроса, больший
//!    читается обратно бОльшим", а не "явный запрос > дефолт". Причина —
//!    в этой песочнице `SO_SNDBUF`/`SO_RCVBUF` без явной установки
//!    авто-тюнятся ядром выше маленького пиннутого запроса, так что "запрос >
//!    дефолт" не сигнализирует "сеттер сработал" — он может пойти в любую
//!    сторону в зависимости от того, насколько агрессивно хост уже
//!    авто-тюнинговал дефолт.
//! 2. Негативные контроли существуют, чтобы позитивные тесты не проходили
//!    против дефолта, который уже совпадает со значением под тестом.
use http_ng_rt::{TcpConnect, TcpOpts};
use http_ng_rt_smol::Smol;
use std::net::{IpAddr, Ipv4Addr};

fn spawn_accepting_listener() -> std::net::SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        let _ = l.accept();
    });
    addr
}

#[test]
fn local_address_selects_the_connecting_source_ip() {
    // 127.0.0.0/8 целиком loopback на Linux, поэтому 127.0.0.2 — валидный,
    // отличный от дефолтного локальный адрес: различает "опция сработала" от
    // "дефолт ОС случайно совпал" (наивный тест против 127.0.0.1 этого сделать
    // не смог бы, поскольку это и есть дефолтный маршрут к 127.0.0.1).
    let addr = spawn_accepting_listener();
    let opts = TcpOpts {
        local_address: Some(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2))),
        ..Default::default()
    };
    futures_executor::block_on(async {
        let s = Smol.connect(addr, &opts).await.expect("connect");
        let local = s.get_ref().local_addr().expect("local_addr query");
        assert_eq!(
            local.ip(),
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)),
            "TcpOpts::local_address did not select the connecting source IP"
        );
    });
}

#[test]
fn default_local_address_is_not_127_0_0_2() {
    // Контроль для теста выше: без опции источник НЕ должен быть 127.0.0.2
    // (иначе предыдущий тест проходил бы, даже если local_address тихо
    // игнорируется, потому что дефолт ОС мог бы случайно совпасть).
    let addr = spawn_accepting_listener();
    futures_executor::block_on(async {
        let s = Smol
            .connect(addr, &TcpOpts::default())
            .await
            .expect("connect");
        let local = s.get_ref().local_addr().expect("local_addr query");
        assert_ne!(local.ip(), IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)));
    });
}

#[test]
fn send_buffer_size_is_applied_before_connect() {
    let small_addr = spawn_accepting_listener();
    let large_addr = spawn_accepting_listener();
    futures_executor::block_on(async {
        let small = Smol
            .connect(
                small_addr,
                &TcpOpts {
                    send_buffer_size: Some(4096),
                    ..Default::default()
                },
            )
            .await
            .expect("small connect");
        let small_size = socket2::SockRef::from(small.get_ref())
            .send_buffer_size()
            .expect("small send_buffer_size query");

        let requested = 1usize << 20; // 1 MiB
        let large = Smol
            .connect(
                large_addr,
                &TcpOpts {
                    send_buffer_size: Some(requested),
                    ..Default::default()
                },
            )
            .await
            .expect("large connect");
        let large_size = socket2::SockRef::from(large.get_ref())
            .send_buffer_size()
            .expect("large send_buffer_size query");

        assert!(
            large_size > small_size,
            "TcpOpts::send_buffer_size did not take effect: requesting {requested} read back as \
             {large_size}, which is not larger than requesting 4096 (read back as {small_size})"
        );
    });
}

#[test]
fn recv_buffer_size_is_applied_before_connect() {
    let small_addr = spawn_accepting_listener();
    let large_addr = spawn_accepting_listener();
    futures_executor::block_on(async {
        let small = Smol
            .connect(
                small_addr,
                &TcpOpts {
                    recv_buffer_size: Some(4096),
                    ..Default::default()
                },
            )
            .await
            .expect("small connect");
        let small_size = socket2::SockRef::from(small.get_ref())
            .recv_buffer_size()
            .expect("small recv_buffer_size query");

        let requested = 1usize << 20; // 1 MiB
        let large = Smol
            .connect(
                large_addr,
                &TcpOpts {
                    recv_buffer_size: Some(requested),
                    ..Default::default()
                },
            )
            .await
            .expect("large connect");
        let large_size = socket2::SockRef::from(large.get_ref())
            .recv_buffer_size()
            .expect("large recv_buffer_size query");

        assert!(
            large_size > small_size,
            "TcpOpts::recv_buffer_size did not take effect: requesting {requested} read back as \
             {large_size}, which is not larger than requesting 4096 (read back as {small_size})"
        );
    });
}

#[test]
fn reuse_address_is_applied_before_connect() {
    let addr = spawn_accepting_listener();
    let opts = TcpOpts {
        reuse_address: true,
        ..Default::default()
    };
    futures_executor::block_on(async {
        let s = Smol.connect(addr, &opts).await.expect("connect");
        let enabled = socket2::SockRef::from(s.get_ref())
            .reuse_address()
            .expect("reuse_address query");
        assert!(enabled, "TcpOpts::reuse_address did not set SO_REUSEADDR");
    });
}

#[test]
fn default_reuse_address_is_off() {
    // Контроль для теста выше.
    let addr = spawn_accepting_listener();
    futures_executor::block_on(async {
        let s = Smol
            .connect(addr, &TcpOpts::default())
            .await
            .expect("connect");
        let enabled = socket2::SockRef::from(s.get_ref())
            .reuse_address()
            .expect("reuse_address query");
        assert!(
            !enabled,
            "SO_REUSEADDR must default to off; TcpOpts::default() must not enable it"
        );
    });
}
