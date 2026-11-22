import 'package:material_ui/material_ui.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';
import 'package:mangayomi/models/chapter.dart';
import 'package:mangayomi/models/history.dart';
import 'package:mangayomi/models/manga.dart';
import 'package:mangayomi/modules/history/providers/isar_providers.dart';
import 'package:mangayomi/modules/library/widgets/library_entry_utils.dart';
import 'package:mangayomi/providers/l10n_providers.dart';
import 'package:mangayomi/repositories/chapter_repository.dart';
import 'package:mangayomi/repositories/manga_repository.dart';
import 'package:mangayomi/repositories/settings_repository.dart';
import 'package:mangayomi/utils/extensions/chapter_extensions.dart';
import 'package:shiori/shiori.dart';

/// Whether libraries open on the "Continue" row.
class ContinueShelfState extends Notifier<bool> {
  @override
  bool build() => settingsRepository.current.showContinueShelf ?? true;

  void set(bool value) {
    state = value;
    settingsRepository.update((s) => s.showContinueShelf = value);
  }
}

final continueShelfProvider = NotifierProvider<ContinueShelfState, bool>(
  ContinueShelfState.new,
);

/// A series to pick up again: where the reader stopped, and how far through
/// the series that is.
class ContinueEntry {
  const ContinueEntry({
    required this.manga,
    required this.chapter,
    required this.progress,
  });

  final Manga manga;
  final Chapter chapter;

  /// Read chapters over all chapters, 0 to 1.
  final double progress;
}

/// The series to offer under "Continue": the most recently read first, each
/// once, only those still in the library, at most [limit].
///
/// [chapterById] and [mangaById] are resolved in one batch by the caller,
/// so building the shelf costs two reads rather than two per row.
List<ContinueEntry> continueEntries(
  List<History> history, {
  required Map<int, Chapter> chapterById,
  required Map<int, Manga> mangaById,
  int limit = 12,
}) {
  int when(History h) => int.tryParse(h.date ?? '') ?? 0;
  final sorted = [...history]..sort((a, b) => when(b).compareTo(when(a)));
  final seen = <int>{};
  final out = <ContinueEntry>[];
  for (final h in sorted) {
    final chapter = chapterById[h.chapterId];
    final manga = mangaById[chapter?.mangaId ?? h.mangaId];
    if (chapter == null || manga == null || manga.id == null) continue;
    if (!(manga.favorite ?? false) || !seen.add(manga.id!)) continue;
    final chapters = manga.chapters.toList();
    final read = chapters.where((c) => c.isRead ?? false).length;
    out.add(
      ContinueEntry(
        manga: manga,
        chapter: chapter,
        progress: chapters.isEmpty ? 0 : read / chapters.length,
      ),
    );
    if (out.length == limit) break;
  }
  return out;
}

/// "Continue" at the top of a library: the series in progress for this
/// media, one tap from the reader. Renders nothing when there is none.
class ContinueShelf extends ConsumerWidget {
  const ContinueShelf({super.key, required this.itemType});

  final ItemType itemType;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    if (!ref.watch(continueShelfProvider)) return const SizedBox.shrink();
    final history = ref.watch(getAllHistoryStreamProvider(itemType: itemType));
    final entries = history.whenOrNull(
      data: (rows) {
        final chapterIds = rows.map((h) => h.chapterId).nonNulls.toList();
        final chapterById = {
          for (final c in chapterRepository.getAllByIds(chapterIds).nonNulls)
            c.id!: c,
        };
        final mangaIds = chapterById.values
            .map((c) => c.mangaId)
            .nonNulls
            .toSet()
            .toList();
        final mangaById = {
          for (final m in mangaRepository.getAllByIds(mangaIds).nonNulls)
            m.id!: m,
        };
        return continueEntries(
          rows,
          chapterById: chapterById,
          mangaById: mangaById,
        );
      },
    );
    if (entries == null || entries.isEmpty) return const SizedBox.shrink();

    return ShioriReadingShelf(
      title: context.l10n.continue_reading,
      openLabel: context.l10n.continue_reading,
      entries: [
        for (final e in entries)
          ShioriReadingEntry(
            id: e.manga.id!,
            title: e.manga.name ?? '',
            subtitle: [
              e.chapter.name ?? '',
              if ((int.tryParse(e.chapter.lastPageRead ?? '') ?? 0) > 0)
                context.l10n.page(e.chapter.lastPageRead!),
            ].where((text) => text.isNotEmpty).join(' · '),
            image: resolveCoverImage(e.manga, ref),
            progress: e.progress,
            onOpen: () => e.chapter.pushToReaderView(context, ignoreIsRead: true),
            onDetails: () =>
                context.push('/manga-reader/detail', extra: e.manga.id),
          ),
      ],
    );
  }
}
