import 'dart:io';

import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:isar_community/isar.dart';
import 'package:mangayomi/models/chapter.dart';
import 'package:mangayomi/models/manga.dart';
import 'package:mangayomi/models/settings.dart';
import 'package:mangayomi/models/source.dart';
import 'package:mangayomi/modules/manga/reader/providers/reader_controller_provider.dart';
import 'package:mangayomi/repositories/db.dart';
import 'package:mangayomi/repositories/db_write_queue.dart';

void main() {
  late Directory directory;
  late ProviderContainer container;
  setUpAll(() async => Isar.initializeIsarCore(download: true));
  setUp(() async {
    directory = await Directory('build/reader-progress-tests')
        .create(recursive: true);
    directory = await directory.createTemp('position-');
    isar = await Isar.open(
      [SettingsSchema, SourceSchema, MangaSchema, ChapterSchema],
      directory: directory.absolute.path,
      name: 'reader_progress',
    );
    isar.writeTxnSync(() {
      isar.settings.putSync(Settings()..incognitoMode = false);
      final manga = Manga(
        id: 1,
        name: 'Fixture',
        favorite: true,
        source: 'Fixture',
        sourceId: 1,
        author: '',
        artist: '',
        genre: [],
        imageUrl: '',
        lang: 'en',
        link: '/fixture',
        status: Status.ongoing,
        description: '',
      );
      isar.mangas.putSync(manga);
      final chapter = Chapter(
        id: 10,
        mangaId: 1,
        name: 'Chapter 1',
        isRead: true,
        lastPageRead: '1',
      )..manga.value = manga;
      isar.chapters.putSync(chapter);
      chapter.manga.saveSync();
    });
    container = ProviderContainer();
  });
  tearDown(() async {
    container.dispose();
    await dbWriteQueue.run(() {});
    await isar.close(deleteFromDisk: true);
    await directory.delete(recursive: true);
  });

  ReaderController reader() => container.read(
    readerControllerProvider(chapter: isar.chapters.getSync(10)!).notifier,
  );

  test(
    'rereading persists each visible page without erasing completion',
    () async {
      final controller = reader();
      controller.setPageIndex(6, false, List.filled(30, 'page'));
      await dbWriteQueue.run(() {});
      final saved = isar.chapters.getSync(10)!;
      expect(saved.isRead, isTrue);
      expect(saved.lastPageRead, '7');
      expect(saved.updatedAt, greaterThan(0));
      container.dispose();
      container = ProviderContainer();
      expect(reader().getPageIndex(), 6);
    },
  );

  test('synced chapter page wins over a stale device-local position', () {
    isar.writeTxnSync(() {
      isar.chapters.putSync(isar.chapters.getSync(10)!..lastPageRead = '18');
      isar.settings.putSync(
        isar.settings.getSync(227)!
          ..chapterPageIndexList = [ChapterPageIndex(chapterId: 10, index: 3)],
      );
    });
    expect(reader().getPageIndex(), 17);
  });

  test('legacy position remains readable and page zero can be saved', () async {
    isar.writeTxnSync(() {
      isar.chapters.putSync(isar.chapters.getSync(10)!..lastPageRead = '');
      isar.settings.putSync(
        isar.settings.getSync(227)!
          ..chapterPageIndexList = [ChapterPageIndex(chapterId: 10, index: 3)],
      );
    });
    final controller = reader();
    expect(controller.getPageIndex(), 3);
    controller.setPageIndex(0, true, List.filled(30, 'page'));
    await dbWriteQueue.run(() {});
    expect(isar.chapters.getSync(10)!.lastPageRead, '1');
    expect(isar.settings.getSync(227)!.chapterPageIndexList, isEmpty);
  });

  test(
    'private reading does not change the saved page or completion',
    () async {
      isar.writeTxnSync(() {
        isar.settings.putSync(
          isar.settings.getSync(227)!..incognitoMode = true,
        );
      });
      final controller = reader();
      expect(controller.getPageIndex(), 0);
      controller.setPageIndex(6, true, List.filled(30, 'page'));
      await dbWriteQueue.run(() {});
      expect(isar.chapters.getSync(10)!.lastPageRead, '1');
      expect(isar.chapters.getSync(10)!.isRead, isTrue);
    },
  );
}
