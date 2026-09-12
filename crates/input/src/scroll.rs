//! The protocol's scroll unit — see ADR-0007's scroll update.
//!
//! `InputMessage::MouseScroll` carries `dx`/`dy` in **1/120 of a wheel
//! notch**: Windows' own `WHEEL_DELTA` unit, which the Windows capture and
//! injector already used natively. A notch is 3 lines, Windows' default
//! (`SPI_GETWHEELSCROLLLINES`), so one line is 40 units.
//!
//! Real hardware: before this unit was defined, each backend used its
//! own — the macOS capture sent whole *lines*, which the Windows injector
//! passed to `SendInput` as 1/120-notch units. The hub log shows 617 scroll
//! events with a mean |dy| of 2: every event scrolled Windows by about
//! 2/120 of a notch, ~60 times too little.

/// One wheel notch, in protocol scroll units (Windows' `WHEEL_DELTA`).
pub const UNITS_PER_NOTCH: i32 = 120;

/// One line, in protocol scroll units: a notch is 3 lines, Windows'
/// default wheel setting.
pub const UNITS_PER_LINE: i32 = UNITS_PER_NOTCH / 3;

/// Converts a (possibly fractional) line delta to protocol units.
pub fn units_from_lines(lines: f64) -> i32 {
    (lines * f64::from(UNITS_PER_LINE)).round() as i32
}

/// Splits `accumulated` units into whole steps of `per_step` units and the
/// remainder, which the caller carries into the next event — so a backend
/// that can only scroll in whole lines or notches still scrolls exactly
/// the total distance, however finely it arrives. Both parts keep the sign
/// of `accumulated`.
pub fn take_whole_steps(accumulated: i32, per_step: i32) -> (i32, i32) {
    (accumulated / per_step, accumulated % per_step)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the slow Mac->Windows scroll: the log's typical
    /// macOS event of 2 lines must become 2 lines' worth of Windows units
    /// (80), not 2 units.
    #[test]
    fn a_mac_line_delta_becomes_the_same_number_of_lines_on_windows() {
        assert_eq!(units_from_lines(2.0), 80);
        assert_eq!(units_from_lines(1.0), UNITS_PER_LINE);
        assert_eq!(units_from_lines(3.0), UNITS_PER_NOTCH);
        assert_eq!(units_from_lines(-2.0), -80);
    }

    #[test]
    fn fractional_trackpad_deltas_are_kept_not_rounded_to_zero() {
        assert_eq!(units_from_lines(0.1), 4);
        assert_eq!(units_from_lines(-0.25), -10);
    }

    #[test]
    fn whole_steps_carry_the_remainder_with_its_sign() {
        assert_eq!(take_whole_steps(100, UNITS_PER_LINE), (2, 20));
        assert_eq!(take_whole_steps(-100, UNITS_PER_LINE), (-2, -20));
        assert_eq!(take_whole_steps(39, UNITS_PER_LINE), (0, 39));
        assert_eq!(take_whole_steps(UNITS_PER_NOTCH, UNITS_PER_NOTCH), (1, 0));
    }

    /// Many small events add up to exactly the whole distance.
    #[test]
    fn small_events_accumulate_to_the_full_distance() {
        let mut remainder = 0;
        let mut lines = 0;
        for _ in 0..10 {
            let (whole, rest) = take_whole_steps(remainder + 4, UNITS_PER_LINE);
            lines += whole;
            remainder = rest;
        }
        assert_eq!((lines, remainder), (1, 0));
    }
}
