//! HIL 控制协议与结构化事件 — 纯逻辑, host 可单测
//!
//! 对应 `docs/HIL.md` 的设备侧控制面约定：
//! - 命令: `PING` / `VERSION` / `SET_VOLUME 60` / `PLAY_TONE 1000 3000` ...
//! - 事件: 每行 `HIL <kind> key=value ...`，供主机 runner 断言。
//!
//! 这里只做解析与格式化，不绑定 UART/USB；`hil-agent` bin 负责收发。

use core::fmt::Write;

/// 事件行最大长度
pub const EVENT_MAX: usize = 192;

/// hil-agent 支持的控制命令
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Ping,
    Version,
    GetCounters,
    ResetCounters,
    /// 设置音量百分比 (0~100)
    SetVolume(u8),
    /// 播放测试音: 频率 Hz (20~20000) + 时长 ms (1~60000)
    PlayTone {
        freq_hz: u32,
        duration_ms: u32,
    },
    /// 采集指定时长并回传统计
    Capture {
        duration_ms: u32,
    },
    Wake,
    BargeIn,
    /// 模拟低电: 电量百分比 (0~100)
    SimLowBattery {
        percent: u8,
    },
    Reboot,
}

/// 解析一行控制命令（大小写不敏感，参数数量必须精确匹配）
///
/// 返回 `None` 表示未知命令、参数缺失/多余或数值越界；调用方应回
/// `HIL error reason=...` 而不是静默忽略。
pub fn parse_command(line: &str) -> Option<Command> {
    let mut args = line.split_ascii_whitespace();
    let cmd = args.next()?;

    if cmd.eq_ignore_ascii_case("PING") {
        return no_more(&mut args).then_some(Command::Ping);
    }
    if cmd.eq_ignore_ascii_case("VERSION") {
        return no_more(&mut args).then_some(Command::Version);
    }
    if cmd.eq_ignore_ascii_case("GET_COUNTERS") {
        return no_more(&mut args).then_some(Command::GetCounters);
    }
    if cmd.eq_ignore_ascii_case("RESET_COUNTERS") {
        return no_more(&mut args).then_some(Command::ResetCounters);
    }
    if cmd.eq_ignore_ascii_case("WAKE") {
        return no_more(&mut args).then_some(Command::Wake);
    }
    if cmd.eq_ignore_ascii_case("BARGE_IN") {
        return no_more(&mut args).then_some(Command::BargeIn);
    }
    if cmd.eq_ignore_ascii_case("REBOOT") {
        return no_more(&mut args).then_some(Command::Reboot);
    }
    if cmd.eq_ignore_ascii_case("SET_VOLUME")
        && let Some(percent) = next_num(&mut args, 0, 100)
        && no_more(&mut args)
    {
        return Some(Command::SetVolume(percent as u8));
    }
    if cmd.eq_ignore_ascii_case("SIM_LOW_BATTERY")
        && let Some(percent) = next_num(&mut args, 0, 100)
        && no_more(&mut args)
    {
        return Some(Command::SimLowBattery {
            percent: percent as u8,
        });
    }
    if cmd.eq_ignore_ascii_case("CAPTURE")
        && let Some(duration_ms) = next_num(&mut args, 1, 60_000)
        && no_more(&mut args)
    {
        return Some(Command::Capture { duration_ms });
    }
    if cmd.eq_ignore_ascii_case("PLAY_TONE")
        && let Some(freq_hz) = next_num(&mut args, 20, 20_000)
        && let Some(duration_ms) = next_num(&mut args, 1, 60_000)
        && no_more(&mut args)
    {
        return Some(Command::PlayTone {
            freq_hz,
            duration_ms,
        });
    }
    None
}

fn next_num<'a>(args: &mut impl Iterator<Item = &'a str>, min: u32, max: u32) -> Option<u32> {
    let value = args.next()?.parse::<u32>().ok()?;
    (min..=max).contains(&value).then_some(value)
}

fn no_more<'a>(args: &mut impl Iterator<Item = &'a str>) -> bool {
    args.next().is_none()
}

/// 关键状态快照事件
#[derive(Debug, Clone, Copy)]
pub enum Event<'a> {
    Boot {
        version: &'a str,
        sha: &'a str,
        psram_bytes: u32,
        reset: &'a str,
    },
    Dma {
        underruns: u32,
        late: u32,
        overflow: u32,
    },
    Wake {
        latency_ms: u32,
        source: &'a str,
    },
    Session {
        state: &'a str,
        frames: u32,
    },
    Tts {
        bytes: u32,
        duration_ms: u32,
        drop: u32,
    },
    Heap {
        internal_used: u32,
        internal_total: u32,
    },
}

/// 格式化为一行 `HIL ...` 事件
///
/// 含空格/控制字符的字符串字段视为非法（会破坏 key=value 解析），
/// 返回 `None`；超出 [`EVENT_MAX`] 时同样返回 `None`。
pub fn format_event(ev: &Event<'_>) -> Option<heapless::String<EVENT_MAX>> {
    let mut out = heapless::String::new();
    write!(out, "HIL ").ok()?;
    match *ev {
        Event::Boot {
            version,
            sha,
            psram_bytes,
            reset,
        } => {
            if !token(version) || !token(sha) || !token(reset) {
                return None;
            }
            write!(
                out,
                "boot version={version} sha={sha} psram={psram_bytes} reset={reset}"
            )
            .ok()?;
        },
        Event::Dma {
            underruns,
            late,
            overflow,
        } => write!(
            out,
            "dma underruns={underruns} late={late} overflow={overflow}"
        )
        .ok()?,
        Event::Wake { latency_ms, source } => {
            if !token(source) {
                return None;
            }
            write!(out, "wake latency_ms={latency_ms} source={source}").ok()?;
        },
        Event::Session { state, frames } => {
            if !token(state) {
                return None;
            }
            write!(out, "session state={state} frames={frames}").ok()?;
        },
        Event::Tts {
            bytes,
            duration_ms,
            drop,
        } => write!(
            out,
            "tts bytes={bytes} duration_ms={duration_ms} drop={drop}"
        )
        .ok()?,
        Event::Heap {
            internal_used,
            internal_total,
        } => write!(
            out,
            "heap internal_used={internal_used} internal_total={internal_total}"
        )
        .ok()?,
    }
    Some(out)
}

/// key=value 字段必须是单个可见 ASCII token
fn token(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b > b' ' && b < 0x7F)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::format;

    #[test]
    fn parses_simple_commands() {
        for (line, want) in [
            ("PING", Command::Ping),
            ("version", Command::Version),
            ("  Get_Counters  ", Command::GetCounters),
            ("RESET_COUNTERS", Command::ResetCounters),
            ("WAKE", Command::Wake),
            ("BARGE_IN", Command::BargeIn),
            ("REBOOT", Command::Reboot),
        ] {
            assert_eq!(parse_command(line), Some(want), "line={line}");
        }
    }

    #[test]
    fn parses_parameterized_commands_and_bounds() {
        assert_eq!(parse_command("SET_VOLUME 60"), Some(Command::SetVolume(60)));
        assert_eq!(parse_command("SET_VOLUME 0"), Some(Command::SetVolume(0)));
        assert_eq!(
            parse_command("SET_VOLUME 100"),
            Some(Command::SetVolume(100))
        );
        assert_eq!(parse_command("SET_VOLUME 101"), None);

        assert_eq!(
            parse_command("PLAY_TONE 1000 3000"),
            Some(Command::PlayTone {
                freq_hz: 1000,
                duration_ms: 3000
            })
        );
        assert_eq!(parse_command("PLAY_TONE 0 3000"), None);
        assert_eq!(parse_command("PLAY_TONE 20001 3000"), None);
        assert_eq!(parse_command("PLAY_TONE 1000 0"), None);
        assert_eq!(parse_command("PLAY_TONE 1000 60001"), None);

        assert_eq!(
            parse_command("CAPTURE 2000"),
            Some(Command::Capture { duration_ms: 2000 })
        );
        assert_eq!(parse_command("CAPTURE 0"), None);
        assert_eq!(
            parse_command("SIM_LOW_BATTERY 15"),
            Some(Command::SimLowBattery { percent: 15 })
        );
        assert_eq!(parse_command("SIM_LOW_BATTERY 101"), None);
    }

    #[test]
    fn rejects_unknown_or_malformed_commands() {
        for line in [
            "",
            "   ",
            "UNKNOWN",
            "PING extra",
            "SET_VOLUME",
            "SET_VOLUME abc",
            "SET_VOLUME -1",
            "SET_VOLUME 60 extra",
            "PLAY_TONE 1000",
            "PLAY_TONE 1000 3000 extra",
            "CAPTURE 1.5",
        ] {
            assert_eq!(parse_command(line), None, "line={line}");
        }
    }

    #[test]
    fn formats_events() {
        assert_eq!(
            format_event(&Event::Boot {
                version: "1.2.3",
                sha: "ab12cd34",
                psram_bytes: 8_388_608,
                reset: "POWER_ON",
            })
            .as_deref(),
            Some("HIL boot version=1.2.3 sha=ab12cd34 psram=8388608 reset=POWER_ON")
        );
        assert_eq!(
            format_event(&Event::Dma {
                underruns: 0,
                late: 0,
                overflow: 0,
            })
            .as_deref(),
            Some("HIL dma underruns=0 late=0 overflow=0")
        );
        assert_eq!(
            format_event(&Event::Wake {
                latency_ms: 820,
                source: "vad",
            })
            .as_deref(),
            Some("HIL wake latency_ms=820 source=vad")
        );
        assert_eq!(
            format_event(&Event::Session {
                state: "Listening",
                frames: 42,
            })
            .as_deref(),
            Some("HIL session state=Listening frames=42")
        );
        assert_eq!(
            format_event(&Event::Tts {
                bytes: 184_320,
                duration_ms: 3000,
                drop: 0,
            })
            .as_deref(),
            Some("HIL tts bytes=184320 duration_ms=3000 drop=0")
        );
        assert_eq!(
            format_event(&Event::Heap {
                internal_used: 81_234,
                internal_total: 131_072,
            })
            .as_deref(),
            Some("HIL heap internal_used=81234 internal_total=131072")
        );
    }

    #[test]
    fn rejects_unparseable_event_fields() {
        assert_eq!(
            format_event(&Event::Wake {
                latency_ms: 1,
                source: "v ad",
            }),
            None
        );
        assert_eq!(
            format_event(&Event::Session {
                state: "",
                frames: 0,
            }),
            None
        );
        // 超长版本号导致整行超过 EVENT_MAX
        let long = "x".repeat(EVENT_MAX);
        assert_eq!(
            format_event(&Event::Boot {
                version: &long,
                sha: "ab",
                psram_bytes: 1,
                reset: "POWER_ON",
            }),
            None
        );
        assert_eq!(format!("{:?}", Command::Ping), "Ping");
    }
}
