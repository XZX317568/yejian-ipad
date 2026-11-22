import 'dart:io';

import 'package:path/path.dart' as p;

/// Creates [directory] and proves it is writable, without deleting anything.
///
/// A read-only data volume or a database left owned by another user otherwise
/// fails later, deep in Isar, with a message that says nothing about where to
/// look. The probe file is removed only when this call created it.
Future<Directory> ensureWritableDirectory(Directory directory) async {
  await directory.create(recursive: true);
  final probe = File(
    p.join(
      directory.path,
      '.yejian_write_probe_${DateTime.now().microsecondsSinceEpoch}',
    ),
  );
  await probe.writeAsString('ok', flush: true);
  await probe.delete();
  return directory;
}
