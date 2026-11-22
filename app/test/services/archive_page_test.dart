import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:archive/archive_io.dart';
import 'package:flutter/painting.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:mangayomi/services/archive_reader/archive_page.dart';
import 'package:mangayomi/services/archive_reader/models/models.dart';
import 'package:mangayomi/services/archive_reader/providers/archive_reader_providers.dart';
import 'package:mangayomi/services/reader/archive_page_image_provider.dart';
import 'package:mangayomi/services/reader/u_chap_data_preload.dart';
import 'package:mangayomi/utils/extensions/others.dart';
import 'package:path/path.dart' as p;

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  late Directory root;
  late ProviderContainer container;
  final cacheDirectories = <String>{};

  setUp(() async {
    root = await Directory.systemTemp.createTemp('archive-pages-test-');
    container = ProviderContainer(retry: (_, _) => null);
  });
  tearDown(() async {
    container.dispose();
    for (final path in cacheDirectories) {
      final dir = Directory(path);
      if (await dir.exists()) await dir.delete(recursive: true);
    }
    cacheDirectories.clear();
    await root.delete(recursive: true);
    PaintingBinding.instance.imageCache.clear();
  });

  Future<LocalArchive> open(File file) async {
    final result = await container.read(
      getArchiveDataFromFileProvider(file.path).future,
    );
    for (final image in result.images!) {
      final page = image.archivePage;
      if (page != null) cacheDirectories.add(p.dirname(page.cachePath));
    }
    return result;
  }

  Future<File> makeArchive(
    String name,
    Map<String, List<int>> entries, {
    bool tar = false,
    int level = 6,
  }) async {
    final archive = Archive();
    for (final entry in entries.entries) {
      archive.addFile(
        ArchiveFile(entry.key, entry.value.length, entry.value)
          ..compression = level == 0 ? CompressionType.none : null,
      );
    }
    return File(p.join(root.path, name)).writeAsBytes(
      tar
          ? TarEncoder().encode(archive)
          : ZipEncoder().encode(archive, level: level),
    );
  }

  test(
    '2700x3494 ZIP indexes pages, then decodes only the requested page',
    () async {
      final recorder = ui.PictureRecorder();
      ui.Canvas(recorder).drawRect(
        const ui.Rect.fromLTWH(0, 0, 2700, 3494),
        ui.Paint()..color = const ui.Color(0xffeeeeee),
      );
      final picture = recorder.endRecording();
      final image = await picture.toImage(2700, 3494);
      final png = (await image.toByteData(format: ui.ImageByteFormat.png))!
          .buffer
          .asUint8List();
      image.dispose();
      picture.dispose();
      final file = await makeArchive('高清 章节.cbz', {
        for (var i = 24; i > 0; i--) 'pages/$i.png': png,
      });
      final archive = await open(file);
      expect(archive.images!.length, 24);
      expect(archive.images!.map((e) => e.name).take(3), [
        '1.png',
        '2.png',
        '3.png',
      ]);
      expect(
        archive.images!.every((e) => e.image == null && e.path == null),
        true,
      );
      expect(archive.coverImage, png);
      expect(Directory(cacheDirectories.single).existsSync(), false);

      final page = archive.images![9].archivePage!;
      final first = page.resolveFilePath();
      final second = page.resolveFilePath();
      expect(identical(first, second), true);
      final path = await first;
      expect(await File(path).readAsBytes(), png);
      expect(Directory(cacheDirectories.single).listSync().length, 1);

      final provider = ArchivePageImageProvider(page);
      final stream = provider.resolve(ImageConfiguration.empty);
      final loaded = Completer<ImageInfo>();
      final listener = ImageStreamListener(
        (info, _) => loaded.complete(info),
        onError: (Object e, StackTrace? s) => loaded.completeError(e, s),
      );
      stream.addListener(listener);
      final info = await loaded.future;
      expect((info.image.width, info.image.height), (2700, 3494));
      stream.removeListener(listener);
      info.dispose();
      await provider.evict();
      expect(Directory(cacheDirectories.single).listSync().length, 1);
    },
  );

  for (final extension in ['zip', 'cbz', 'tar', 'cbt']) {
    test(
      '$extension nested duplicate basenames stay distinct and confined',
      () async {
        final file = await makeArchive('test.$extension', {
          'a/1.jpeg': [1, 2, 3],
          'b/1.WEBP': [4, 5, 6],
          '../../outside.png': [7, 8, 9],
          '.hidden.png': [0],
        }, tar: extension == 'tar' || extension == 'cbt');
        final archive = await open(file);
        expect(archive.images!.length, 3);
        final paths = <String>[];
        for (final image in archive.images!) {
          paths.add(await image.archivePage!.resolveFilePath());
        }
        expect(paths.toSet().length, 3);
        expect(
          paths.every((path) => p.isWithin(cacheDirectories.single, path)),
          true,
        );
        final a = archive.images!.firstWhere(
          (e) => e.archivePage!.entryName == 'a/1.jpeg',
        );
        expect(
          await File(await a.archivePage!.resolveFilePath()).readAsBytes(),
          [1, 2, 3],
        );
      },
    );
  }

  test(
    'missing/truncated cache is rebuilt; source replacement invalidates it',
    () async {
      final file = await makeArchive('replace.zip', {
        '1.png': [1, 2, 3],
      });
      final archive = await open(file);
      final page = archive.images!.single.archivePage!;
      final data = UChapDataPreload(
        null,
        null,
        null,
        true,
        null,
        0,
        null,
        0,
        archivePage: page,
      );
      final path = (await data.getLocalFilePath)!;
      data.resolvedFilePath = path;
      await File(path).writeAsBytes([0]);
      expect(await data.getImageBytes, [1, 2, 3]);
      await File(path).delete();
      expect(await data.getImageBytes, [1, 2, 3]);
      await makeArchive('replace.zip', {
        '1.png': [4, 5, 6, 7],
      });
      await expectLater(
        data.getLocalFilePath,
        throwsA(isA<FileSystemException>()),
      );
      final refreshed = await container.refresh(
        getArchiveDataFromFileProvider(file.path).future,
      );
      final newPage = refreshed.images!.single.archivePage!;
      cacheDirectories.add(p.dirname(newPage.cachePath));
      expect(newPage.cachePath, isNot(page.cachePath));
      expect(await File(await newPage.resolveFilePath()).readAsBytes(), [
        4,
        5,
        6,
        7,
      ]);
    },
  );

  test(
    'damaged page fails without poisoning the cache or later page requests',
    () async {
      final file = await makeArchive('damaged.zip', {
        '1.png': [1, 2, 3],
        '2.png': [4, 5, 6],
        '3.png': [7, 8, 9],
      }, level: 0);
      final bytes = await file.readAsBytes();
      _corruptStoredEntry(bytes, '2.png');
      await file.writeAsBytes(bytes);
      final archive = await open(file);
      final badPage = archive.images![1].archivePage!;
      await expectLater(
        badPage.resolveFilePath(),
        throwsA(isA<FormatException>()),
      );
      expect(File(badPage.cachePath).existsSync(), false);
      expect(File('${badPage.cachePath}.part').existsSync(), false);
      final goodPage = archive.images![2].archivePage!;
      expect(await File(await goodPage.resolveFilePath()).readAsBytes(), [
        7,
        8,
        9,
      ]);
      await expectLater(
        badPage.resolveFilePath(),
        throwsA(isA<FormatException>()),
      );
    },
  );

  test(
    'folder contract stays path-backed and damaged covers fail visibly',
    () async {
      final folder = Directory(p.join(root.path, 'folder'))..createSync();
      File(p.join(folder.path, '2.png')).writeAsBytesSync([2]);
      File(p.join(folder.path, '10.png')).writeAsBytesSync([10]);
      final archive = await container.read(
        getArchiveDataFromFileProvider(folder.path).future,
      );
      expect(archive.images!.map((e) => e.name), ['2.png', '10.png']);
      expect(
        archive.images!.every(
          (e) => e.path != null && e.archivePage == null && e.image == null,
        ),
        true,
      );
      final file = await makeArchive('cover.zip', {
        '1.png': [1, 2, 3],
      }, level: 0);
      final bytes = await file.readAsBytes();
      _corruptStoredEntry(bytes, '1.png');
      await file.writeAsBytes(bytes);
      await expectLater(
        container.read(getArchivesDataFromFileProvider(file.path).future),
        throwsA(isA<FormatException>()),
      );
    },
  );

  test(
    'archive cache trim is bounded, protects recent pages, and keeps sources',
    () async {
      final cache = Directory(p.join(root.path, 'cache-root'))
        ..createSync(recursive: true);
      final source = File(p.join(root.path, 'source.cbz'))
        ..writeAsBytesSync([1, 2, 3]);
      final oldPage = File(p.join(cache.path, 'old', '1.jpg'))
        ..createSync(recursive: true)
        ..writeAsBytesSync(List.filled(64, 1));
      final recentPage = File(p.join(cache.path, 'recent', '2.jpg'))
        ..createSync(recursive: true)
        ..writeAsBytesSync(List.filled(64, 2));
      final stalePartial = File(p.join(cache.path, 'stale', '3.jpg.part'))
        ..createSync(recursive: true)
        ..writeAsBytesSync(List.filled(64, 3));
      final freshPartial = File(p.join(cache.path, 'fresh', '3.jpg.part'))
        ..createSync(recursive: true)
        ..writeAsBytesSync(List.filled(64, 4));
      final now = DateTime.now();
      oldPage.setLastModifiedSync(now.subtract(const Duration(hours: 2)));
      recentPage.setLastModifiedSync(now);
      stalePartial.setLastModifiedSync(now.subtract(const Duration(hours: 2)));
      freshPartial.setLastModifiedSync(now);

      await ArchivePageCache.trimDirectory(
        cache,
        maxBytes: 64,
        protectRecent: const Duration(minutes: 10),
        stalePartialAge: const Duration(hours: 1),
      );

      expect(await oldPage.exists(), false);
      expect(await recentPage.exists(), true);
      expect(await stalePartial.exists(), false);
      expect(await freshPartial.exists(), true);
      expect(await source.readAsBytes(), [1, 2, 3]);
    },
  );
}

void _corruptStoredEntry(Uint8List bytes, String name) {
  final view = ByteData.sublistView(bytes);
  for (var i = 0; i + 30 < bytes.length; i++) {
    if (view.getUint32(i, Endian.little) != 0x04034b50) continue;
    final nameSize = view.getUint16(i + 26, Endian.little);
    final extraSize = view.getUint16(i + 28, Endian.little);
    if (utf8.decode(bytes.sublist(i + 30, i + 30 + nameSize)) != name) continue;
    bytes[i + 30 + nameSize + extraSize] ^= 0xff;
    return;
  }
  throw StateError('Test entry not found: $name');
}
