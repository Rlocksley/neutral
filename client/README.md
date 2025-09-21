# Client UI Theme

This client uses a unified egui theme with a blue/orange palette and consistent control sizes.

## Theme tokens
- UI_HEIGHT: 36.0 — standard height for interactive controls (buttons, combo boxes, etc.)
- BUTTON_WIDTH: 120.0 — default button width
- RADIUS: 8.0 — rounded corner radius
- Primary Blue: #1976D2 (hover #1E88E5, dark #1565C0)
- Accent Orange: #FF9800 (dark #E68200)
- Surfaces: dark greys

These are defined near the top of `src/main.rs` and applied via `configure_theme(&egui::Context)`.

## Notes
- The multiline message input is intentionally taller for usability and is not constrained to UI_HEIGHT.
- Outgoing chat bubbles use primary blue with white text; incoming bubbles use a subtle dark surface with a blue border.
- Selection and active accents use blue/orange to avoid any green hues.

## Tweaking
- Adjust color constants in `configure_theme`.
- To change global control height, update `style.spacing.interact_size.y` (set by UI_HEIGHT).
- To standardize more component sizes, prefer `ui.add_sized([width, UI_HEIGHT], widget)`.
