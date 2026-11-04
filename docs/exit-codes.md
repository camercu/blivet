<!-- markdownlint-disable-file MD041 -->
<!-- Included by the ErrorKind rustdoc; checked by src/error.rs. -->

| `ErrorKind` variant | Exit code | `sysexits.h` constant |
| ------------------- | --------- | --------------------- |
| `ValidationError`        | 64        | `EX_USAGE`            |
| `ProgramNotFound`        | 66        | `EX_NOINPUT`          |
| `UserNotFound`           | 67        | `EX_NOUSER`           |
| `GroupNotFound`          | 67        | `EX_NOUSER`           |
| `LockConflict`           | 69        | `EX_UNAVAILABLE`      |
| `LockfileError`          | 73        | `EX_CANTCREAT`        |
| `ForkFailed`             | 71        | `EX_OSERR`            |
| `SetsidFailed`           | 71        | `EX_OSERR`            |
| `ChdirFailed`            | 71        | `EX_OSERR`            |
| `SystemError`            | 71        | `EX_OSERR`            |
| `PermissionDenied`       | 77        | `EX_NOPERM`           |
| `PidfileError`           | 73        | `EX_CANTCREAT`        |
| `OutputFileError`        | 73        | `EX_CANTCREAT`        |
| `ChownError`             | 73        | `EX_CANTCREAT`        |
| `ExecFailed`             | 71        | `EX_OSERR`            |
| `NotifyFailed`           | 71        | `EX_OSERR`            |
| `InitLeftThreads`        | 70        | `EX_SOFTWARE`         |
| `Application`            | caller's `code`; 0 remapped to 70 | (caller's choice) |
