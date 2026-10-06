# Code preferences

Mandatory for the agent on Rust it writes or is already changing. Not
`check.sh` failures. Do not rewrite unrelated sites in a file just to apply
a preference.

- Use `buf[X..Y].try_into()` instead of `[buf[X], buf[X+1], buf[X+2], ... buf[Y-1]]`.
  `buf` is any slice; the target is any `[T; N]`, including `from_be_bytes` /
  `from_le_bytes`.
- Use human-readable snake_case names for locals, parameters, match bindings,
  and closure arguments. Do not use one-letter names. Short conventional words
  (`len`, `buf`, `idx`, `ok`) are fine. This does not apply to type parameters,
  lifetimes, `_`, or struct fields.
