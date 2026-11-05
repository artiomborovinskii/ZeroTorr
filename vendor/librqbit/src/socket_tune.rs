//! ZeroTorr: клэмп буферов сокетов пиров (ТЗ 1.4.5, раздел 4.1).
//!
//! Ядро Linux по умолчанию даёт TCP-сокету сотни килобайт — мегабайты буферов
//! (tcp_rmem/tcp_wmem); при 15–20 пирах это десятки МБ RAM на устройстве со
//! 130 МБ свободными. Здесь на каждый пировой сокет ставим TCP_NODELAY и, в
//! режиме low-memory, жёсткие потолки SO_RCVBUF=64 КиБ / SO_SNDBUF=32 КиБ:
//! 20 пиров съедят менее 2 МБ вместо ~40 МБ.
//!
//! Флаг глобальный (AtomicBool): у процесса одна сессия librqbit, выставляется
//! из SessionOptions::low_memory_sockets до старта слушателя и коннектора.
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};

static LOW_MEMORY_SOCKETS: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_low_memory(v: bool) {
    LOW_MEMORY_SOCKETS.store(v, Ordering::Relaxed);
}

/// Применить опции к любому fd (сокет целиком, ReadHalf или WriteHalf — это
/// один и тот же сокет, setsockopt работает через любой дескриптор).
pub(crate) fn tune<F: AsRawFd>(s: &F) {
    let fd = s.as_raw_fd();
    unsafe {
        // TCP_NODELAY: мелкие keep-alive/ interested-сообщения не ждут Nagle
        let one: libc::c_int = 1;
        libc::setsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_NODELAY,
            &one as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
        if LOW_MEMORY_SOCKETS.load(Ordering::Relaxed) {
            let rcv: libc::c_int = 64 * 1024;
            let snd: libc::c_int = 32 * 1024;
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                &rcv as *const libc::c_int as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                &snd as *const libc::c_int as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
    }
}
