//! 简易 imgcat：使用 iTerm2 Inline Images Protocol 把图片字节流写到控制台。
//!
//! 协议格式：`ESC ] 1337 ; File = inline=1 : <base64> BEL`
//!
//! 终端支持情况：
//! - 支持：iTerm2、WezTerm、mintty、tio、kitty (兼容模式) 等。
//! - 不支持：picocom / 裸 cat —— 会把 base64 当作乱码输出（无害但不显示图）。
//!
//! 串口直接送出，不经过 tmux/screen，因此使用裸 OSC 1337 序列即可。
//! 流式编码 + 局部缓冲，避免分配大块内存。

#![allow(dead_code)]

use ax_hal::console::write_bytes;

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// 一次刷出多少 base64 字节到控制台（必须是 4 的倍数）。
const FLUSH_BYTES: usize = 1024;

/// 把任意字节流（通常是 JPEG/PNG）以 iTerm2 inline image 协议写入控制台。
pub fn print_image(image: &[u8]) {
    write_bytes(b"\x1b]1337;File=inline=1:");
    base64_stream(image);
    write_bytes(b"\x07\n");
}

/// 仅写出 base64 编码后的内容（不带协议头/尾），方便复用。
fn base64_stream(input: &[u8]) {
    let mut buf = [0u8; FLUSH_BYTES];
    let mut pos = 0usize;

    let mut chunks = input.chunks_exact(3);
    for c in chunks.by_ref() {
        let n = ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | (c[2] as u32);
        buf[pos] = ALPHABET[((n >> 18) & 0x3f) as usize];
        buf[pos + 1] = ALPHABET[((n >> 12) & 0x3f) as usize];
        buf[pos + 2] = ALPHABET[((n >> 6) & 0x3f) as usize];
        buf[pos + 3] = ALPHABET[(n & 0x3f) as usize];
        pos += 4;
        if pos == FLUSH_BYTES {
            write_bytes(&buf);
            pos = 0;
        }
    }

    let rem = chunks.remainder();
    match rem.len() {
        1 => {
            let n = (rem[0] as u32) << 16;
            buf[pos] = ALPHABET[((n >> 18) & 0x3f) as usize];
            buf[pos + 1] = ALPHABET[((n >> 12) & 0x3f) as usize];
            buf[pos + 2] = b'=';
            buf[pos + 3] = b'=';
            pos += 4;
        }
        2 => {
            let n = ((rem[0] as u32) << 16) | ((rem[1] as u32) << 8);
            buf[pos] = ALPHABET[((n >> 18) & 0x3f) as usize];
            buf[pos + 1] = ALPHABET[((n >> 12) & 0x3f) as usize];
            buf[pos + 2] = ALPHABET[((n >> 6) & 0x3f) as usize];
            buf[pos + 3] = b'=';
            pos += 4;
        }
        _ => {}
    }

    if pos > 0 {
        write_bytes(&buf[..pos]);
    }
}
