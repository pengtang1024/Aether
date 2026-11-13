//! One-line status strings for the RPC config and speed presets.

use crate::i18n;

/// One line describing the daemon's persisted RPC configuration. The
/// listener only rebinds at daemon start, so the wording avoids claiming
/// the address is live right now.
pub(crate) fn rpc_status_text(listen: &str, has_secret: bool) -> String {
    let listen = listen.trim();
    if listen.is_empty() {
        return i18n::tr("远程控制未开启", "Remote control is off");
    }
    if has_secret {
        i18n::tr(
            format!("已配置 {listen}，需要令牌（重启 daemon 后按此地址监听）"),
            format!(
                "Configured on {listen} with a token (the daemon listens there after a restart)"
            ),
        )
    } else {
        i18n::tr(
            format!("已配置 {listen}，但缺少访问令牌：TCP 不会监听（请填写令牌或留空关闭）"),
            format!(
                "Configured on {listen} without a token: the TCP listener stays off (set a token or clear the address)"
            ),
        )
    }
}

/// Speed-limit pill → bytes per second (`0` = unlimited).
pub(crate) fn speed_limit_bps(mode: i32) -> u64 {
    match mode {
        1 => 1_000_000,
        2 => 5_000_000,
        3 => 20_000_000,
        _ => 0,
    }
}

/// Nearest pill index for a stored cap (the settings slider can set any KB/s).
pub(crate) fn speed_limit_mode(bps: u64) -> i32 {
    let presets: [u64; 4] = [0, 1_000_000, 5_000_000, 20_000_000];
    (0..presets.len())
        .min_by_key(|&index| presets[index].abs_diff(bps))
        .unwrap_or(0) as i32
}
