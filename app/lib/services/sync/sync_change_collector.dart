// Gathering local changes for an upload, and clearing them once a sync
// confirms they were applied. The mirror direction of SyncEntityApplier:
// local rows -> wire, instead of wire -> local rows.
import 'dart:convert';
import 'dart:math';

import 'package:flutter/foundation.dart' show visibleForTesting;

import 'package:mangayomi/models/category.dart';
import 'package:mangayomi/models/changed.dart';
import 'package:mangayomi/models/chapter.dart';
import 'package:mangayomi/models/history.dart';
import 'package:mangayomi/models/manga.dart';
import 'package:mangayomi/models/settings.dart';
import 'package:mangayomi/models/sync_preference.dart';
import 'package:mangayomi/models/track.dart';
import 'package:mangayomi/models/update.dart';
import 'package:mangayomi/providers/sync_providers.dart';
import 'package:mangayomi/repositories/category_repository.dart';
import 'package:mangayomi/repositories/chapter_repository.dart';
import 'package:mangayomi/repositories/history_repository.dart';
import 'package:mangayomi/repositories/manga_repository.dart';
import 'package:mangayomi/repositories/settings_repository.dart';
import 'package:mangayomi/repositories/track_repository.dart';
import 'package:mangayomi/repositories/update_repository.dart';
import 'package:mangayomi/services/sync/sync_wire_types.dart';
import 'package:mangayomi_sync/mangayomi_sync.dart';

// Local reader-only settings that make no sense on another device (paths,
// device-specific proxy/JRE locations). Stripped before upload, and
// preserved (not overwritten) when applying a pulled settings blob - see
// SyncEntityApplier._preserveDeviceLocalSettings.
const _deviceLocalSettingsKeys = {
  'localFolders',
  'namedLocalFolders',
  'downloadLocalFolderName',
  'askDownloadDestination',
  'androidProxyServer',
  'jrePath',
  'extensionServerPath',
  'updatesLastSeenAt',
  'libraryLastCategoryIdMangas',
  'libraryLastCategoryIdAnime',
  'libraryLastCategoryIdNovel',
};

class ChangedRows {
  final List<Map<String, dynamic>> categories;
  final List<Map<String, dynamic>> manga;
  final List<Map<String, dynamic>> chapters;
  final List<Map<String, dynamic>> tracks;
  final List<Map<String, dynamic>> histories;
  final List<Map<String, dynamic>> updates;
  final Map<String, dynamic>? deleted;
  final Map<String, dynamic>? settings;
  final List<ChangedPartVersion> _deletionVersionsToAck;

  /// Local rows left out of the upload: a value the other end would refuse,
  /// and that cannot be shortened without changing what it means.
  final List<UnsentSyncRow> unsent;

  ChangedRows._({
    required this.categories,
    required this.manga,
    required this.chapters,
    required this.tracks,
    required this.histories,
    required this.updates,
    required this.deleted,
    required this.settings,
    required this._deletionVersionsToAck,
    required this.unsent,
  });

  /// The rows made to fit what the other end takes (see [fitSyncRow]). A
  /// row that cannot fit is left out and listed in [unsent]: one such row
  /// would otherwise have every sync of this device refused.
  @visibleForTesting
  factory ChangedRows.fromRows({
    required List<Map<String, dynamic>> categories,
    required List<Map<String, dynamic>> manga,
    required List<Map<String, dynamic>> chapters,
    required List<Map<String, dynamic>> tracks,
    required List<Map<String, dynamic>> histories,
    required List<Map<String, dynamic>> updates,
    required Map<String, dynamic>? deleted,
    required Map<String, dynamic>? settings,
    required List<ChangedPartVersion> deletionVersionsToAck,
  }) {
    final unsent = <UnsentSyncRow>[];
    final blocked = <String, Set<int>>{};
    List<Map<String, dynamic>> fit(
      String entity,
      List<Map<String, dynamic>> rows,
    ) {
      final result = <Map<String, dynamic>>[];
      for (final row in rows) {
        final fitted = fitSyncRow(entity, row);
        final blockedParent =
            blocked['manga']?.contains(row['mangaClientId']) == true
            ? 'mangaClientId'
            : blocked['chapters']?.contains(row['chapterClientId']) == true
            ? 'chapterClientId'
            : null;
        final field = fitted.tooLong ?? blockedParent;
        if (field == null) {
          result.add(fitted.row!);
        } else {
          final name = row['name'] ?? row['title'] ?? row['chapterName'];
          unsent.add(
            UnsentSyncRow(entity, name is String ? name : null, field),
          );
          final id = row['clientId'];
          if (id is int) (blocked[entity] ??= {}).add(id);
        }
      }
      return result;
    }

    return ChangedRows._(
      categories: fit('categories', categories),
      manga: fit('manga', manga),
      chapters: fit('chapters', chapters),
      tracks: fit('tracks', tracks),
      histories: fit('histories', histories),
      updates: fit('updates', updates),
      deleted: deleted,
      settings: settings,
      deletionVersionsToAck: deletionVersionsToAck,
      unsent: unsent,
    );
  }

  /// Normal incremental sync: rows changed since the local watermark,
  /// plus pending local deletions.
  factory ChangedRows.collect(SyncPreference prefs, Synching notifier) {
    final since = localUploadSince(prefs);

    final deleted = <String, List<int>>{};
    final deletionVersions = <ChangedPartVersion>[];
    void collectDeleted(String key, ActionType action) {
      final parts = notifier.getChangedParts([action]);
      final ids = parts.map((p) => p.clientId).nonNulls.toSet().toList();
      if (ids.isNotEmpty) deleted[key] = ids;
      // Keep the exact rows, not just their action types. A deletion made
      // while this upload is in flight must not be cleared with them.
      deletionVersions.addAll(parts.map(ChangedPartVersion.of));
    }

    collectDeleted('categories', ActionType.removeCategory);
    collectDeleted('manga', ActionType.removeItem);
    collectDeleted('chapters', ActionType.removeChapter);
    collectDeleted('tracks', ActionType.removeTrack);
    collectDeleted('histories', ActionType.removeHistory);
    collectDeleted('updates', ActionType.removeUpdate);

    Map<String, dynamic>? settingsWire;
    final current = settingsRepository.current;
    if ((current.updatedAt ?? 0) >= since) {
      settingsWire = _settingsToWire(current);
    }

    return ChangedRows.fromRows(
      categories:
          (since == 0
                  ? categoryRepository.getAll()
                  : categoryRepository.getChangedSince(since))
              .map(_categoryToWire)
              .toList(),
      manga:
          (since == 0
                  ? mangaRepository.getAll()
                  : mangaRepository.getChangedSince(since))
              .map((m) => _mangaToWire(m))
              .nonNulls
              .toList(),
      chapters:
          (since == 0
                  ? chapterRepository.getAll()
                  : chapterRepository.getChangedSince(since))
              .map((c) => _chapterToWire(c))
              .nonNulls
              .toList(),
      tracks:
          (since == 0
                  ? trackRepository.getAll()
                  : trackRepository.getChangedSince(since))
              .map((t) => _trackToWire(t))
              .nonNulls
              .toList(),
      histories:
          (since == 0
                  ? historyRepository.getAll()
                  : historyRepository.getChangedSince(since))
              .map((h) => _historyToWire(h))
              .nonNulls
              .toList(),
      updates:
          (since == 0
                  ? updateRepository.getAll()
                  : updateRepository.getChangedSince(since))
              .map((u) => _updateToWire(u))
              .nonNulls
              .toList(),
      deleted: deleted.isEmpty ? null : deleted,
      settings: settingsWire,
      deletionVersionsToAck: deletionVersions,
    );
  }

  /// "Upload only": every local row, every entity, with `updatedAt` forced
  /// so it wins regardless of what the server holds. No deletions - see the
  /// comment on SyncServer._forceUpload for why.
  factory ChangedRows.collectAll({required int forcedUpdatedAt}) {
    final settingsWire = _settingsToWire(
      settingsRepository.current,
      forcedUpdatedAt: forcedUpdatedAt,
    );
    return ChangedRows.fromRows(
      categories: categoryRepository
          .getAll()
          .map((c) => {..._categoryToWire(c), 'updatedAt': forcedUpdatedAt})
          .toList(),
      manga: mangaRepository
          .getAll()
          .map((m) => _mangaToWire(m, forcedUpdatedAt: forcedUpdatedAt))
          .nonNulls
          .toList(),
      chapters: chapterRepository
          .getAll()
          .map((c) => _chapterToWire(c, forcedUpdatedAt: forcedUpdatedAt))
          .nonNulls
          .toList(),
      tracks: trackRepository
          .getAll()
          .map((t) => _trackToWire(t, forcedUpdatedAt: forcedUpdatedAt))
          .nonNulls
          .toList(),
      histories: historyRepository
          .getAll()
          .map((h) => _historyToWire(h, forcedUpdatedAt: forcedUpdatedAt))
          .nonNulls
          .toList(),
      updates: updateRepository
          .getAll()
          .map((u) => _updateToWire(u, forcedUpdatedAt: forcedUpdatedAt))
          .nonNulls
          .toList(),
      deleted: null,
      settings: settingsWire,
      deletionVersionsToAck: const [],
    );
  }

  /// Parents finish uploading before their children. Independent list
  /// slices can send a chapter before the manga in a later slice; hosts
  /// correctly reject that orphan and the old client then forgets it.
  /// Keep ample room below the host's 25 MiB limit for request metadata.
  List<Map<String, dynamic>> chunked(
    int maxRows, {
    int maxBytes = 8 * 1024 * 1024,
  }) {
    if (maxRows < 1 || maxBytes < 1024) {
      throw ArgumentError('Invalid batch limit');
    }
    final chunks = <Map<String, dynamic>>[];
    final blockedManga = <int>{};
    final blockedChapters = <int>{};
    final entities = {
      'categories': categories,
      'manga': manga,
      'chapters': chapters,
      'tracks': tracks,
      'histories': histories,
      'updates': updates,
    };
    for (final entry in entities.entries) {
      var batch = <Map<String, dynamic>>[];
      var bytes = 128;
      for (final row in entry.value) {
        final size = utf8.encode(jsonEncode(row)).length + 1;
        final blockedParent = blockedManga.contains(row['mangaClientId'])
            ? 'mangaClientId'
            : blockedChapters.contains(row['chapterClientId'])
            ? 'chapterClientId'
            : null;
        if (size + 128 > maxBytes || blockedParent != null) {
          final name = row['name'] ?? row['title'] ?? row['chapterName'];
          unsent.add(
            UnsentSyncRow(
              entry.key,
              name is String ? name : null,
              blockedParent ?? 'request size',
            ),
          );
          if (entry.key == 'manga') blockedManga.add(row['clientId'] as int);
          if (entry.key == 'chapters') {
            blockedChapters.add(row['clientId'] as int);
          }
          continue;
        }
        if (batch.length >= maxRows || bytes + size > maxBytes) {
          chunks.add({entry.key: batch});
          batch = [];
          bytes = 128;
        }
        batch.add(row);
        bytes += size;
      }
      if (batch.isNotEmpty) chunks.add({entry.key: batch});
    }
    // Deletions need batching too, and must run after upserts so a later
    // upload chunk cannot resurrect a row deleted by the same snapshot.
    for (final entry
        in deleted?.entries ?? const <MapEntry<String, dynamic>>[]) {
      final ids = entry.value as List;
      for (var start = 0; start < ids.length; start += maxRows) {
        chunks.add({
          'deleted': {
            entry.key: ids.sublist(start, min(start + maxRows, ids.length)),
          },
        });
      }
    }
    if (settings != null) {
      if (utf8.encode(jsonEncode(settings)).length + 128 <= maxBytes) {
        chunks.add({'settings': settings});
      } else {
        unsent.add(const UnsentSyncRow('settings', null, 'request size'));
      }
    }
    return chunks.isEmpty ? [{}] : chunks;
  }

  Future<void> acknowledgeUploaded(Synching notifier) async {
    if (_deletionVersionsToAck.isEmpty) return;
    await notifier.acknowledgeChangedParts(_deletionVersionsToAck, true);
  }
}

/// A local row left out of an upload, for telling the user which one.
class UnsentSyncRow {
  const UnsentSyncRow(this.entity, this.name, this.field);

  final String entity;

  /// The row's name or title, when it has one.
  final String? name;

  /// The field over its limit.
  final String field;

  @override
  String toString() {
    final name = this.name;
    return name == null || name.isEmpty ? '$entity.$field' : '"$name" ($field)';
  }
}

/// The server cursor is on the host's clock and is only for downloads.
/// A detected local clock rollback starts a full upload to avoid omitting
/// edits made after the clock changed. Conflict ordering is still LWW.
int localUploadSince(SyncPreference prefs, {int? now}) {
  if ((prefs.since ?? 0) == 0) return 0;
  final last = prefs.lastSync ?? 0;
  return last > (now ?? DateTime.now().millisecondsSinceEpoch) ? 0 : last;
}

Map<String, dynamic> _categoryToWire(Category c) => {
  'clientId': c.clientId,
  'name': c.name,
  'forItemType': wireItemTypes[c.forItemType.index],
  'pos': c.pos,
  'hide': c.hide ?? false,
  'shouldUpdate': c.shouldUpdate ?? true,
  'updatedAt': c.updatedAt ?? 0,
};

// Returns null when the row can't be represented on the wire (no clientId
// yet - shouldn't happen once backfill has run, but skip rather than crash
// if it somehow does).
Map<String, dynamic>? _mangaToWire(Manga m, {int? forcedUpdatedAt}) {
  if (m.clientId == null) return null;
  return {
    'clientId': m.clientId,
    'source': m.source,
    'sourceId': m.sourceId,
    'link': m.link,
    'itemType': wireItemTypes[m.itemType.index],
    'name': m.name,
    'imageUrl': m.imageUrl,
    'description': m.description,
    'author': m.author,
    'artist': m.artist,
    'status': wireStatuses[m.status.index],
    'genre': m.genre ?? [],
    'lang': m.lang,
    'lastUpdate': m.lastUpdate,
    'favorite': m.favorite ?? false,
    'dateAdded': m.dateAdded,
    'lastRead': m.lastRead,
    'isLocalArchive': m.isLocalArchive ?? false,
    'customCoverFromTracker': m.customCoverFromTracker,
    'smartUpdateDays': m.smartUpdateDays,
    'categoryClientIds': (m.categories ?? [])
        .map((id) => categoryRepository.findById(id)?.clientId)
        .nonNulls
        .toList(),
    'updatedAt': forcedUpdatedAt ?? m.updatedAt ?? 0,
  };
}

Map<String, dynamic>? _chapterToWire(Chapter c, {int? forcedUpdatedAt}) {
  final mangaClientId = c.mangaId == null
      ? null
      : mangaRepository.findById(c.mangaId!)?.clientId;
  if (c.clientId == null || mangaClientId == null) return null;
  return {
    'clientId': c.clientId,
    'mangaClientId': mangaClientId,
    'name': c.name,
    'url': c.url,
    'scanlator': c.scanlator,
    'dateUpload': int.tryParse(c.dateUpload ?? ''),
    'isFiller': c.isFiller ?? false,
    'thumbnailUrl': c.thumbnailUrl,
    'description': c.description,
    'downloadSize': int.tryParse(c.downloadSize ?? ''),
    'duration': int.tryParse(c.duration ?? ''),
    'isRead': c.isRead ?? false,
    'isBookmarked': c.isBookmarked ?? false,
    'lastPageRead': c.lastPageRead,
    'updatedAt': forcedUpdatedAt ?? c.updatedAt ?? 0,
  };
}

Map<String, dynamic>? _trackToWire(Track t, {int? forcedUpdatedAt}) {
  final mangaClientId = t.mangaId == null
      ? null
      : mangaRepository.findById(t.mangaId!)?.clientId;
  if (t.clientId == null || mangaClientId == null) return null;
  return {
    'clientId': t.clientId,
    'mangaClientId': mangaClientId,
    'syncId': t.syncId,
    'mediaId': t.mediaId,
    'libraryId': t.libraryId,
    'title': t.title,
    'lastChapterRead': t.lastChapterRead,
    'totalChapter': t.totalChapter,
    'score': t.score,
    'status': wireTrackStatuses[t.status.index],
    'startedReadingDate': t.startedReadingDate,
    'finishedReadingDate': t.finishedReadingDate,
    'trackingUrl': t.trackingUrl,
    'itemType': wireItemTypes[t.itemType.index],
    'updatedAt': forcedUpdatedAt ?? t.updatedAt ?? 0,
  };
}

Map<String, dynamic>? _historyToWire(History h, {int? forcedUpdatedAt}) {
  final mangaClientId = h.mangaId == null
      ? null
      : mangaRepository.findById(h.mangaId!)?.clientId;
  final chapterClientId = h.chapterId == null
      ? null
      : chapterRepository.findByIdSync(h.chapterId!)?.clientId;
  if (h.clientId == null || mangaClientId == null || chapterClientId == null) {
    return null;
  }
  return {
    'clientId': h.clientId,
    'mangaClientId': mangaClientId,
    'chapterClientId': chapterClientId,
    'itemType': wireItemTypes[h.itemType.index],
    'date': int.tryParse(h.date ?? ''),
    'readingTimeSeconds': h.readingTimeSeconds ?? 0,
    'updatedAt': forcedUpdatedAt ?? h.updatedAt ?? 0,
  };
}

Map<String, dynamic>? _updateToWire(Update u, {int? forcedUpdatedAt}) {
  final mangaClientId = u.mangaId == null
      ? null
      : mangaRepository.findById(u.mangaId!)?.clientId;
  if (u.clientId == null || mangaClientId == null) return null;
  return {
    'clientId': u.clientId,
    'mangaClientId': mangaClientId,
    'chapterName': u.chapterName,
    'date': int.tryParse(u.date ?? ''),
    'updatedAt': forcedUpdatedAt ?? u.updatedAt ?? 0,
  };
}

Map<String, dynamic> _settingsToWire(
  Settings settings, {
  int? forcedUpdatedAt,
}) {
  final json = settings.toJson();
  json['updatedAt'] = forcedUpdatedAt ?? json['updatedAt'] ?? 0;
  // Session cookies are device/login-specific, never worth syncing.
  json['cookiesList'] = [];
  for (final key in _deviceLocalSettingsKeys) {
    json.remove(key);
  }
  return {'data': json, 'updatedAt': json['updatedAt']};
}
