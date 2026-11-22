/// A failed file must not roll back or prevent the other files in a batch.
class LocalImportFailure {
  const LocalImportFailure(this.path, this.error);

  final String path;
  final Object error;
}

class LocalImportResult {
  final succeeded = <String>[];
  final failures = <LocalImportFailure>[];
}

/// Runs one file at a time so metadata/cover reads don't multiply memory use.
/// The caller owns each file's transaction; the result preserves failed paths
/// and errors for reporting instead of silently skipping a damaged file.
Future<LocalImportResult> importLocalFiles<T>(
  Iterable<T> files, {
  required String Function(T file) pathOf,
  required Future<void> Function(T file) importFile,
}) async {
  final result = LocalImportResult();
  for (final file in files) {
    try {
      await importFile(file);
      result.succeeded.add(pathOf(file));
    } catch (error) {
      result.failures.add(LocalImportFailure(pathOf(file), error));
    }
  }
  return result;
}
