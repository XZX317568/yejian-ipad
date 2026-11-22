import 'dart:async';
import 'dart:io';
import 'dart:isolate';
import 'dart:typed_data';

import 'package:archive/archive_io.dart';
import 'package:flutter/foundation.dart';
import 'package:path/path.dart' as p;

/// Page-by-page archive extraction writes decoded entries under a private
/// system-temporary directory. Keep that cache bounded without touching the
/// archive the user opened.
abstract final class ArchivePageCache {
  static const int defaultMaxBytes = 512 * 1024 * 1024;
  static const Duration defaultProtectRecent = Duration(minutes: 10);
  static const Duration defaultStalePartialAge = Duration(hours: 1);

  static DateTime? _lastTrim;
  static bool _trimming = false;

  static Directory get root =>
      Directory(p.join(Directory.systemTemp.path, 'mangayomi_archive_pages'));

  static Future<int> size() async {
    final directory = root;
    if (!await directory.exists()) return 0;
    var total = 0;
    try {
      await for (final entity in directory.list(
        recursive: true,
        followLinks: false,
      )) {
        if (entity is File) {
          try {
            total += await entity.length();
          } catch (_) {}
        }
      }
    } catch (_) {}
    return total;
  }

  /// Deletes only this module's own cache root, never an archive or a caller
  /// supplied path.
  static Future<int> clear() async {
    final directory = root;
    if (!await directory.exists()) return 0;
    var deleted = 0;
    try {
      await for (final entity in directory.list(
        recursive: true,
        followLinks: false,
      )) {
        if (entity is File) deleted++;
      }
      await directory.delete(recursive: true);
    } catch (_) {}
    return deleted;
  }

  /// Bounds disk use after a page has been resolved. Files touched within
  /// [protectRecent] are kept so the visible page and its prefetch neighbours
  /// are not pulled out from under the reader. Stale `.part` files left by a
  /// killed isolate are removed separately.
  static Future<void> trim({int? maxBytes}) => _trimDirectory(
    root,
    maxBytes: maxBytes ?? defaultMaxBytes,
    protectRecent: defaultProtectRecent,
    stalePartialAge: defaultStalePartialAge,
  );

  /// Throttles the directory walk so scrolling does not scan the cache for
  /// every page.
  static Future<void> maybeTrim() async {
    final now = DateTime.now();
    if (_trimming) return;
    final last = _lastTrim;
    if (last != null && now.difference(last) < const Duration(minutes: 2)) {
      return;
    }
    _trimming = true;
    _lastTrim = now;
    try {
      await trim();
    } finally {
      _trimming = false;
    }
  }

  @visibleForTesting
  static Future<void> trimDirectory(
    Directory directory, {
    int maxBytes = defaultMaxBytes,
    Duration protectRecent = defaultProtectRecent,
    Duration stalePartialAge = defaultStalePartialAge,
  }) => _trimDirectory(
    directory,
    maxBytes: maxBytes,
    protectRecent: protectRecent,
    stalePartialAge: stalePartialAge,
  );

  static Future<void> _trimDirectory(
    Directory directory, {
    required int maxBytes,
    required Duration protectRecent,
    required Duration stalePartialAge,
  }) async {
    if (!await directory.exists()) return;
    final rootPath = p.normalize(directory.absolute.path);
    final now = DateTime.now();
    final files = <({File file, int size, DateTime modified})>[];
    var total = 0;
    try {
      await for (final entity in directory.list(
        recursive: true,
        followLinks: false,
      )) {
        if (entity is! File) continue;
        final path = p.normalize(entity.absolute.path);
        if (!p.isWithin(rootPath, path)) continue;
        try {
          final stat = await entity.stat();
          final isPartial = entity.path.endsWith('.part');
          if (isPartial &&
              now.difference(stat.modified) >= stalePartialAge) {
            await entity.delete();
            continue;
          }
          total += stat.size;
          files.add((file: entity, size: stat.size, modified: stat.modified));
        } catch (_) {}
      }
    } catch (_) {
      return;
    }

    if (total <= maxBytes) return;
    files.sort((a, b) => a.modified.compareTo(b.modified));
    for (final entry in files) {
      if (total <= maxBytes) break;
      // Recent files may belong to the page being read or prefetched.
      if (now.difference(entry.modified) < protectRecent) continue;
      try {
        await entry.file.delete();
        total -= entry.size;
      } catch (_) {}
    }
  }
}

/// An archive entry, without its image bytes. Only the requested page is
/// streamed to disk, in a worker isolate, before an image decoder opens it.
class ArchivePage {
  const ArchivePage({
    required this.archivePath,
    required this.entryIndex,
    required this.entryName,
    required this.size,
    required this.archiveSize,
    required this.archiveModified,
    required this.cachePath,
    required this.isTar,
  });

  final String archivePath;
  final int entryIndex;
  final String entryName;
  final int size;
  final int archiveSize;
  final int archiveModified;
  final String cachePath;
  final bool isTar;

  // Reader prefetch, thumbnails and the visible page can request the same
  // entry together. Coalesce those requests and bound decompression to one
  // worker, including when adjacent chapters are being prefetched.
  static final _pending = <String, Future<String>>{};
  static Future<void> _queue = Future<void>.value();

  Future<String> resolveFilePath() {
    final pending = _pending[cachePath];
    if (pending != null) return pending;
    final result = _queue.then((_) => Isolate.run(() => _extractPage(this)));
    _pending[cachePath] = result;
    _queue = result.then<void>(
      (_) {
        _pending.remove(cachePath);
        unawaited(ArchivePageCache.maybeTrim());
      },
      onError: (Object error, StackTrace stack) {
        _pending.remove(cachePath);
        unawaited(ArchivePageCache.maybeTrim());
      },
    );
    return result;
  }
}

Archive decodePageArchive(InputFileStream input, {required bool isTar}) => isTar
    ? TarDecoder().decodeStream(input, verify: true)
    : ZipDecoder().decodeStream(input, verify: true);

Uint8List readArchiveCover(ArchiveFile entry) {
  final bytes = entry.content;
  if (bytes.length != entry.size ||
      (entry.crc32 != null && getCrc32(bytes) != entry.crc32)) {
    throw FormatException('Damaged archive cover: ${entry.name}');
  }
  return bytes;
}

String _extractPage(ArchivePage page) {
  final stat = File(page.archivePath).statSync();
  if (stat.size != page.archiveSize ||
      stat.modified.microsecondsSinceEpoch != page.archiveModified) {
    throw FileSystemException(
      'Archive changed while reading. Reopen the chapter.',
      page.archivePath,
    );
  }
  final cached = File(page.cachePath);
  if (cached.existsSync() && cached.lengthSync() == page.size) {
    _touch(cached);
    return cached.path;
  }

  final input = InputFileStream(page.archivePath);
  File? partial;
  try {
    final archive = decodePageArchive(input, isTar: page.isTar);
    final entry = archive.files[page.entryIndex];
    if (!entry.isFile ||
        entry.isSymbolicLink ||
        entry.name != page.entryName ||
        entry.size != page.size) {
      throw const FormatException('Archive page no longer matches its index.');
    }
    cached.parent.createSync(recursive: true);
    // No archive-provided name is used as an output path.
    partial = File('${cached.path}.part');
    final output = OutputFileStream(partial.path);
    try {
      entry.writeContent(output);
    } finally {
      output.closeSync();
    }
    if (partial.lengthSync() != page.size || page.size == 0) {
      throw FormatException('Incomplete archive page: ${page.entryName}');
    }
    // archive 4.5.2 ignores ZipDecoder.verify. Check the requested page in
    // bounded chunks, without retaining its full encoded bytes in Dart.
    if (entry.crc32 != null) {
      final file = partial.openSync();
      var crc = 0;
      try {
        final buffer = Uint8List(64 * 1024);
        int count;
        while ((count = file.readIntoSync(buffer)) > 0) {
          crc = getCrc32(Uint8List.sublistView(buffer, 0, count), crc);
        }
      } finally {
        file.closeSync();
      }
      if (crc != entry.crc32) {
        throw FormatException('Damaged archive page: ${page.entryName}');
      }
    }
    if (cached.existsSync()) cached.deleteSync();
    partial.renameSync(cached.path);
    _touch(cached);
    return cached.path;
  } finally {
    input.closeSync();
    if (partial != null && partial.existsSync()) partial.deleteSync();
  }
}

void _touch(File file) {
  try {
    file.setLastModifiedSync(DateTime.now());
  } catch (_) {}
}

/// Opaque index-based names also keep nested duplicate basenames distinct.
String archivePageCachePath(String cacheDirectory, int index, String name) =>
    p.join(cacheDirectory, '$index${p.extension(name).toLowerCase()}');
