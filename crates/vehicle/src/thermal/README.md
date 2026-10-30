# thermal

The Zenmuse XT2 thermal core's settings.

| File | Purpose |
|------|---------|
| `../thermal.rs` | the ordered settings queue (spot meter, display mode, isotherm band, measurement type), bounded, with the writer's take/sent/retry protocol |
| `auto.rs` | auto range: keeps the isotherm band fitted to the scene from the full-frame area measurement, once a second, until a manual setting ends it |
