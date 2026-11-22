import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:app_links/app_links.dart';
import 'package:archive/archive.dart';
import 'package:bot_toast/bot_toast.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/gestures.dart';
import 'package:material_ui/material_ui.dart';
import 'package:shiori/shiori.dart';
import 'package:go_router/go_router.dart';
import 'package:flutter_inappwebview/flutter_inappwebview.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:intl/date_symbol_data_local.dart';
import 'package:mangayomi/eval/model/m_bridge.dart';
import 'package:mangayomi/models/custom_button.dart';
import 'package:mangayomi/models/manga.dart';
import 'package:mangayomi/models/settings.dart';
import 'package:mangayomi/models/source.dart';
import 'package:mangayomi/repositories/custom_button_repository.dart';
import 'package:mangayomi/repositories/db.dart';
import 'package:mangayomi/repositories/settings_repository.dart';
import 'package:mangayomi/repositories/track_repository.dart';
export 'package:mangayomi/repositories/db.dart';
export 'package:mangayomi/services/http/m_client.dart'
    show webViewEnvironment, customDns;
import 'package:mangayomi/models/track.dart' as track;
import 'package:mangayomi/modules/manga/detail/providers/track_state_providers.dart';
import 'package:mangayomi/modules/more/data_and_storage/providers/storage_usage.dart';
import 'package:mangayomi/modules/more/settings/browse/providers/browse_state_provider.dart';
import 'package:mangayomi/modules/more/settings/general/providers/general_state_provider.dart';
import 'package:mangayomi/providers/l10n_providers.dart';
import 'package:mangayomi/modules/onboarding/onboarding_screen.dart';
import 'package:mangayomi/modules/onboarding/providers/onboarding_state_provider.dart';
import 'package:mangayomi/providers/storage_provider.dart';
import 'package:mangayomi/router/router.dart';
import 'package:mangayomi/modules/more/settings/appearance/providers/theme_mode_state_provider.dart';
import 'package:mangayomi/l10n/generated/app_localizations.dart';
import 'package:mangayomi/l10n/l10n_delegates.dart';
import 'package:mangayomi/utils/legacy_material.dart';
import 'package:mangayomi/services/library_updater.dart';
import 'package:mangayomi/services/sync_server.dart';
import 'package:mangayomi/services/http/m_client.dart';
import 'package:mangayomi/services/m_extension_server.dart';
import 'package:mangayomi/services/download_manager/m_downloader.dart';
import 'package:mangayomi/src/rust/frb_generated.dart';
import 'package:mangayomi/utils/discord_rpc.dart';
import 'package:mangayomi/services/crash_native.dart';
import 'package:mangayomi/services/crash_report.dart';
import 'package:mangayomi/utils/log/logger.dart';
import 'package:mangayomi/utils/client_id.dart';
import 'package:mangayomi/utils/platform_utils.dart';
import 'package:mangayomi/utils/url_protocol/api.dart';
import 'package:mangayomi/modules/more/settings/appearance/providers/theme_provider.dart';
import 'package:mangayomi/modules/library/providers/file_scanner.dart';
import 'package:mangayomi/utils/app_lock.dart';
import 'package:mangayomi/services/secure_screen.dart';
import 'package:mangayomi/modules/manga/download/providers/download_provider.dart';
import 'package:flutter_foreground_task/flutter_foreground_task.dart';
import 'package:mangayomi/services/background_work/background_work.dart';
import 'package:mangayomi/services/background_work/download_work.dart';
import 'package:mangayomi/repositories/chapter_repository.dart';
import 'package:mangayomi/utils/extensions/chapter_extensions.dart';
import 'package:mangayomi/services/download_manager/download_gate.dart';
import 'package:mangayomi/modules/more/settings/security/providers/security_state_provider.dart';
import 'package:mangayomi/modules/more/settings/security/app_lock_screen.dart';
import 'package:media_kit/media_kit.dart';
import 'package:path_provider/path_provider.dart';
import 'package:window_manager/window_manager.dart';
import 'package:path/path.dart' as p;
import 'package:flutter/services.dart' show rootBundle, LogicalKeyboardKey;
import 'package:mangayomi/utils/window_geometry.dart';
import 'package:mangayomi/modules/more/settings/general/providers/memory_probe_provider.dart';
import 'package:mangayomi/modules/widgets/memory_overlay.dart';
import 'package:mangayomi/modules/widgets/app_ui_scale.dart';
import 'package:mangayomi/modules/more/settings/appearance/providers/app_ui_scale_state_provider.dart';
import 'package:mangayomi/utils/error_toast.dart';
import 'package:mangayomi/core/app_identity.dart';
import 'package:mangayomi/core/startup_failure.dart';
import 'package:mangayomi/services/startup_controller.dart';
import 'package:mangayomi/modules/startup/startup_recovery_screen.dart';
export 'package:mangayomi/services/discord_rpc.dart' show discordRpc;

void main(List<String> args) async {
  StartupController? startup;
  // Zone-level catch-all for anything that slips through both layers
  runZonedGuarded(
    () async {
      WidgetsFlutterBinding.ensureInitialized();
      registerShioriLicenses();

      // Cap the decoded image cache so a large library grid can't fill the
      // default 100 MB ceiling with full-resolution covers and OOM constrained
      // mobile heaps. Mobile gets a tight 64 MB; desktop keeps 256 MB.
      PaintingBinding.instance.imageCache.maximumSizeBytes = isMobile
          ? 64 << 20
          : 256 << 20;

      // Widget-layer errors (build / layout / paint)
      FlutterError.onError = (FlutterErrorDetails details) {
        FlutterError.presentError(details); // keep default red-screen in debug
        AppLogger.log(
          'FlutterError: ${details.exceptionAsString()}\n${details.stack}',
          logLevel: LogLevel.error,
        );
        CrashReports.record(
          source: 'FlutterError',
          error: details.exceptionAsString(),
          stack: details.stack,
        );
      };

      // Async errors that escape the Flutter framework (PlatformDispatcher)
      PlatformDispatcher.instance.onError = (Object error, StackTrace stack) {
        AppLogger.log(
          'PlatformDispatcher error: $error\n$stack',
          logLevel: LogLevel.error,
        );
        CrashReports.record(
          source: 'PlatformDispatcher',
          error: error,
          stack: stack,
        );
        startup?.reportUnhandled(error, stack);
        return true; // handled — prevent app termination
      };

      final storage = StorageProvider();
      startup = StartupController(
        initialize: (controller) => _initializeApplication(storage, controller),
      );
      // Give the launch screen a frame before native initialization, so a
      // Rust or database failure is visible instead of a blank window.
      runApp(_BootstrapApp(controller: startup!));
      unawaited(startup!.start());
    },
    (Object error, StackTrace stack) {
      AppLogger.log(
        'runZonedGuarded error: $error\n$stack',
        logLevel: LogLevel.error,
      );
      CrashReports.record(
        source: 'runZonedGuarded',
        error: error,
        stack: stack,
      );
      startup?.reportUnhandled(error, stack);
    },
  );
}

Future<void> _initializeApplication(
  StorageProvider storage,
  StartupController controller,
) async {
  await _startupStage(controller, StartupStage.rust, () async {
    MediaKit.ensureInitialized();
    // A previous attempt may have half-loaded the bridge. Disposing its Dart
    // state is safe and is what makes Retry a real retry.
    try {
      RustLib.dispose();
    } catch (_) {}
    await RustLib.init();
    // Detect Android TV / leanback so the UI can branch on form factor.
    // No-op on other platforms. See #729.
    await initIsTv();
  });

  await _startupStage(controller, StartupStage.database, () async {
    // Expensive worker isolates start lazily on first use instead of delaying
    // the first frame.
    if (!isMobile) {
      await windowManager.ensureInitialized();
      await WindowGeometry.restore();
    }
    if (Platform.isWindows) {
      registerProtocolHandler(AppIdentity.linkScheme);
    }
    if (Platform.isAndroid) FlutterForegroundTask.initCommunicationPort();

    // Logs stay with the visible reader data.
    try {
      final directory = await storage.getDefaultDirectory();
      await CrashReports.init(directory);
      // After CrashReports, because a native crash from the last run is
      // recorded into it.
      await NativeCrashHandler.init(directory);
    } catch (error, stack) {
      // Crash reporting is best-effort and never blocks opening the reader.
      recordError(error, stack: stack, source: 'startup');
    }

    // The recovery screen shows the database location, because that is the
    // step that failed. Resolving it must not create or migrate anything.
    try {
      final databaseDirectory = await storage.resolveDatabaseDirectory();
      controller.setDataPath(databaseDirectory.path);
    } catch (error, stack) {
      recordError(error, stack: stack, source: 'startup.dataPath');
    }

    try {
      isar = await storage.initDB(null, inspector: kDebugMode);
    } catch (error, stack) {
      // Close a handle that opened before a later step failed; never delete it.
      try {
        await isar.close(deleteFromDisk: false);
      } catch (_) {}
      Error.throwWithStackTrace(error, stack);
    }
  });

  unawaited(_postLaunchInit(storage));
}

Future<void> _startupStage(
  StartupController controller,
  StartupStage stage,
  Future<void> Function() body,
) async {
  try {
    await body();
  } catch (error, stack) {
    AppLogger.log(
      '${stage.name} startup failed: $error\n$stack',
      logLevel: LogLevel.error,
    );
    CrashReports.record(
      source: 'startup.${stage.name}',
      error: error,
      stack: stack,
    );
    controller.fail(stage, error, stack);
  }
}

class _BootstrapApp extends StatelessWidget {
  const _BootstrapApp({required this.controller});

  final StartupController controller;

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: controller,
      builder: (context, _) {
        if (controller.ready) {
          return ProviderScope(
            child: const MyApp(),
            retry: (retryCount, error) => null,
          );
        }
        final failure = controller.failure;
        return MaterialApp(
          title: AppIdentity.englishName,
          debugShowCheckedModeBanner: false,
          theme: ThemeData(
            colorScheme: ColorScheme.fromSeed(
              seedColor: const Color(0xFF5B6B5A),
            ),
            useMaterial3: true,
          ),
          darkTheme: ThemeData(
            colorScheme: ColorScheme.fromSeed(
              seedColor: const Color(0xFF9DB39A),
              brightness: Brightness.dark,
            ),
            useMaterial3: true,
          ),
          home: failure != null
              ? StartupRecoveryScreen(
                  failure: failure,
                  dataPath: controller.dataPath ?? '尚无法确定，请先重试一次',
                  onRetry: () => unawaited(controller.start()),
                  retrying: controller.running,
                )
              : const _LaunchingScreen(),
        );
      },
    );
  }
}

class _LaunchingScreen extends StatelessWidget {
  const _LaunchingScreen();

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Scaffold(
      body: Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(
              Symbols.menu_book_rounded,
              size: 56,
              color: theme.colorScheme.primary,
            ),
            const SizedBox(height: 16),
            Text(
              AppIdentity.productName,
              style: theme.textTheme.headlineSmall?.copyWith(
                fontWeight: FontWeight.w600,
              ),
            ),
            const SizedBox(height: 8),
            Text(
              '正在打开书库…',
              style: theme.textTheme.bodySmall?.copyWith(
                color: theme.colorScheme.onSurfaceVariant,
              ),
            ),
            const SizedBox(height: 24),
            const SizedBox(
              width: 22,
              height: 22,
              child: CircularProgressIndicator(strokeWidth: 2),
            ),
          ],
        ),
      ),
    );
  }
}

/// The tracker library cache moved from Hive to Isar. Remove the box Hive
/// left behind, in the directory `Hive.initFlutter` used to open it.
Future<void> _deleteLegacyHiveCache() async {
  try {
    // Only this app's database directory. An upstream Mangayomi install, if
    // present, owns its own Hive files and they are left alone.
    final dir = (await StorageProvider().getDatabaseDirectory())!.path;
    for (final name in ["tracker_library.hive", "tracker_library.lock"]) {
      final file = File(p.join(dir, name));
      if (await file.exists()) await file.delete();
    }
  } catch (error, stackTrace) {
    recordError(error, stack: stackTrace, source: 'main');
  }
}

Future<void> _postLaunchInit(StorageProvider storage) async {
  await AppLogger.init();
  // Backfills clientId on rows saved before that field existed. Runs on every
  // launch rather than gating on a version check - once caught up it's just
  // six empty indexed lookups, so there's no real cost to checking again.
  unawaited(backfillMissingClientIds());
  unawaited(MDownloader.initializeIsolatePool(poolSize: 6));
  unawaited(_deleteLegacyHiveCache());
  // A new install turns Discord off. Do not even load the plugin then.
  final discordEnabled =
      settingsRepository.currentOrNull?.enableDiscordRpc ?? false;
  if (isDesktop && !kDebugMode && discordEnabled) {
    discordRpc = DiscordRPC(applicationId: "1395040506677039157");
    await discordRpc?.initialize();
  }
  await storage.deleteBtDirectory();
  await webviewServer();
  // Deferred until after runApp() creates the window: on Windows,
  // WebViewEnvironment.create() needs COM initialized on a thread with an
  // active message pump, which doesn't exist yet during main()'s pre-launch
  // setup. Running it here (post-first-frame territory) avoids the
  // "CoInitialize has not been called" PlatformException.
  if (!kIsWeb && defaultTargetPlatform == TargetPlatform.windows) {
    final availableVersion = await WebViewEnvironment.getAvailableVersion();
    if (availableVersion != null) {
      final document = await getApplicationDocumentsDirectory();
      webViewEnvironment = await WebViewEnvironment.create(
        settings: WebViewEnvironmentSettings(
          userDataFolder: p.join(document.path, 'flutter_inappwebview'),
        ),
      );
    }
  }
}

class MyApp extends ConsumerStatefulWidget {
  const MyApp({super.key});

  @override
  ConsumerState<MyApp> createState() => _MyAppState();
}

class _MyAppState extends ConsumerState<MyApp>
    with WidgetsBindingObserver, WindowListener {
  late AppLinks _appLinks;
  StreamSubscription<Uri>? _linkSubscription;
  Uri? lastUri;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    if (!isMobile) windowManager.addListener(this);
    initializeDateFormatting();
    ref.listenManual(
      secureScreenOnProvider,
      (_, secure) => SecureScreen.apply(secure),
      fireImmediately: true,
    );
    _initBackgroundWork();
    customDns = ref.read(customDnsStateProvider);
    _initDeepLinks();
    _setupMpvConfig();

    // Tracker refresh and the local-library filesystem scan compete with the
    // first paint for network/CPU; run them shortly after the UI is up.
    WidgetsBinding.instance.addPostFrameCallback((_) {
      Future.delayed(const Duration(seconds: 2), () {
        if (!mounted) return;
        _checkTrackerRefresh();
        unawaited(ref.read(scanLocalLibraryProvider.future));
      });
    });

    // The scheduled library refresh and auto-sync, when due. They go last and
    // stay quiet: launch is already busy, and these walk data or hit network.
    WidgetsBinding.instance.addPostFrameCallback((_) {
      Future.delayed(const Duration(seconds: 5), () {
        if (!mounted) return;
        unawaited(_autoUpdateLibrary());
        unawaited(autoSyncIfDue(ref));
      });
    });

    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!Platform.isIOS ||
          ref.read(autoStartExtensionServerOnLaunchStateProvider)) {
        MExtensionServerPlatform(ref).startServer();
      }
      if (ref.read(clearChapterCacheOnAppLaunchStateProvider)) {
        // Watch before calling clearcache to keep it alive, so that _getTotalDiskSpace completes safely
        ref.watch(totalChapterCacheSizeStateProvider);
        ref
            .read(totalChapterCacheSizeStateProvider.notifier)
            .clearCache(showToast: false);
      }
    });
  }

  /// When the app last left the screen, for the lock delay.
  DateTime? _backgroundedAt;

  /// Whether the app is off screen or on its way, on iOS, where a secure
  /// screen is covered so the app switcher's preview shows nothing.
  bool _obscured = false;

  /// The scheduled library refresh, when due; starts the download queue when
  /// it queued new chapters.
  Future<void> _autoUpdateLibrary() async {
    final result = await autoUpdateLibraryIfDue(ref);
    if (!mounted || (result?.queued ?? 0) == 0) return;
    ref.invalidate(processDownloadsProvider());
    ref.read(processDownloadsProvider());
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    super.didChangeAppLifecycleState(state);
    // iOS takes the app switcher's preview as the app goes inactive.
    if (Platform.isIOS) {
      final obscured = state != AppLifecycleState.resumed;
      if (obscured != _obscured) setState(() => _obscured = obscured);
    }
    if (state == AppLifecycleState.paused ||
        state == AppLifecycleState.hidden) {
      if (Platform.isLinux) {
        return;
      }
      // Lock the app when going to background (if lock is enabled), or note
      // when it left for a lock delay to count from.
      _backgroundedAt ??= DateTime.now();
      final lockEnabled = ref.read(appLockEnabledStateProvider);
      if (lockEnabled && ref.read(appLockDelayStateProvider) == 0) {
        ref.read(appUnlockedStateProvider.notifier).lock();
      }
    } else if (state == AppLifecycleState.resumed) {
      if (ref.read(appLockEnabledStateProvider) &&
          shouldLockOnResume(
            delayMinutes: ref.read(appLockDelayStateProvider),
            backgroundedAt: _backgroundedAt,
            now: DateTime.now(),
          )) {
        ref.read(appUnlockedStateProvider.notifier).lock();
      }
      _backgroundedAt = null;
      // Launch is the other trigger for the scheduled refresh, so without this
      // a session that stays open for days - a desktop one, typically - would
      // never run one. The interval check makes this a no-op the rest of the
      // time.
      unawaited(_autoUpdateLibrary());
      unawaited(autoSyncIfDue(ref));
    }
  }

  @override
  Widget build(BuildContext context) {
    final followSystem = ref.watch(followSystemThemeStateProvider);
    final forcedDark = ref.watch(themeModeStateProvider);
    final themeMode = followSystem
        ? ThemeMode.system
        : (forcedDark ? ThemeMode.dark : ThemeMode.light);
    final locale = ref.watch(l10nLocaleStateProvider);
    final router = ref.watch(routerProvider);

    return MaterialApp.router(
      theme: ref.watch(lightThemeProvider),
      darkTheme: ref.watch(darkThemeProvider),
      themeMode: themeMode,
      debugShowCheckedModeBanner: false,
      locale: locale,
      localizationsDelegates: appLocalizationsDelegates,
      supportedLocales: AppLocalizations.supportedLocales,
      builder: (context, child) {
        Widget content = child ?? const SizedBox.shrink();
        // First launch of a fresh install: the app has no sources of its own,
        // so say so before dropping the user into an empty Browse. Gating here
        // rather than in the router keeps the TV shortcuts, the UI scale and
        // the toast host below wrapping it exactly as they wrap everything else.
        // Fading rather than swapping. The first run used to vanish in a
        // single frame onto whatever the router had underneath it, which is
        // also the frame the browse branch is still building in, so the end of
        // the flow was the roughest part of it.
        final onboarding = !ref.watch(onboardingCompletedStateProvider);
        content = AnimatedSwitcher(
          duration: const Duration(milliseconds: 380),
          // The app fades up over most of the window while the first run
          // holds, then leaves. Crossing them evenly showed both at half
          // strength through the middle, which reads as a flicker.
          switchInCurve: const Interval(0.35, 1, curve: Curves.easeOut),
          switchOutCurve: const Interval(0, 0.5, curve: Curves.easeIn),
          child: onboarding
              ? const OnboardingScreen()
              : KeyedSubtree(key: const ValueKey('app'), child: content),
        );
        // The theme's ground behind every page: a page transition fades the
        // page it leaves, and the window's black showed through on a light
        // theme.
        content = ColoredBox(
          color: Theme.of(context).scaffoldBackgroundColor,
          child: content,
        );
        // On TV, a single-line text field consumes Up/Down for the text cursor,
        // trapping focus so the remote can't reach the surrounding buttons (a
        // dialog's Cancel/Add, etc.). Remap Up/Down to move focus app-wide: a
        // no-op everywhere except inside a text field, where it frees the field.
        if (isTv) {
          content = Shortcuts(
            shortcuts: const <ShortcutActivator, Intent>{
              SingleActivator(LogicalKeyboardKey.arrowDown):
                  DirectionalFocusIntent(TraversalDirection.down),
              SingleActivator(LogicalKeyboardKey.arrowUp):
                  DirectionalFocusIntent(TraversalDirection.up),
            },
            child: content,
          );
        }
        // Normalize the TV UI to a fixed reference width so it looks consistent
        // across TVs regardless of the density the device reports. No-op off-TV.
        final scaledChild = AppUiScale(
          scale: ref.watch(appUiScaleStateProvider),
          child: content,
        );
        // bot_toast draws its toasts on the old built-in Material library,
        // as do a few other packages under it.
        final base = LegacyMaterialBridge(
          child: BotToastInit()(context, scaledChild),
        );
        final withBackHandler = !isMobile
            ? _MouseBackButtonHandler(router: router, child: base)
            : base;

        if (_obscured && ref.watch(secureScreenOnProvider)) {
          return Stack(
            fit: StackFit.expand,
            children: [
              withBackHandler,
              ColoredBox(color: Theme.of(context).scaffoldBackgroundColor),
            ],
          );
        }

        if (!Platform.isLinux) {
          final isUnlocked = ref.watch(appUnlockedStateProvider);
          final lockEnabled = ref.watch(appLockEnabledStateProvider);
          if (lockEnabled && !isUnlocked) {
            return Stack(
              fit: StackFit.expand,
              children: [withBackHandler, const AppLockScreen()],
            );
          }
        }

        // Sits above everything, including the lock screen, because a
        // measurement is not worth taking if navigating away ends it.
        if (ref.watch(memoryOverlayVisibleProvider)) {
          return Stack(
            fit: StackFit.expand,
            children: [
              withBackHandler,
              MemoryOverlay(
                probe: ref.read(memoryProbeProvider),
                onClose: () =>
                    ref.read(memoryOverlayVisibleProvider.notifier).set(false),
              ),
            ],
          );
        }

        return withBackHandler;
      },
      routeInformationParser: router.routeInformationParser,
      routerDelegate: router.routerDelegate,
      routeInformationProvider: router.routeInformationProvider,
      title: AppIdentity.productName,
      scrollBehavior: AllowScrollBehavior(),
    );
  }

  /// The notification shown while the app works in the background, and
  /// what its buttons do.
  void _initBackgroundWork() {
    DownloadWork.instance.start();
    FlutterForegroundTask.addTaskDataCallback(BackgroundWork.handleAction);
    BackgroundWork.onAction = (action) {
      switch (action) {
        case 'stop_library_update':
          ref.read(libraryUpdateProvider.notifier).requestCancel();
        case 'cancel_downloads':
          for (final id in scheduledDownloads.value) {
            chapterRepository.findByIdSync(id)?.cancelDownloads(id);
          }
      }
    };
  }

  @override
  void dispose() {
    DownloadWork.instance.dispose();
    FlutterForegroundTask.removeTaskDataCallback(BackgroundWork.handleAction);
    WidgetsBinding.instance.removeObserver(this);
    if (!isMobile) {
      windowManager.removeListener(this);
      WindowGeometry.save();
    }
    MExtensionServerPlatform(ref).stopServer();
    _linkSubscription?.cancel();
    discordRpc?.destroy();
    stopwebviewServer();
    AppLogger.dispose();
    super.dispose();
  }

  // Linux emits `resize`/`move` while the window is being resized or moved;
  // Windows and macOS emit `resized`/`moved` once it settles. Handle both, or
  // the geometry is never saved on Linux and the window size is not remembered.
  @override
  void onWindowResize() => WindowGeometry.save();

  @override
  void onWindowResized() => WindowGeometry.save();

  @override
  void onWindowMove() => WindowGeometry.save();

  @override
  void onWindowMoved() => WindowGeometry.save();

  @override
  void onWindowClose() {
    WindowGeometry.save();
    // Workaround for libepoxy error when closing app; caused by media-kit
    if (Platform.isLinux) exit(0);
  }

  Future<void> _initDeepLinks() async {
    _appLinks = AppLinks();
    _linkSubscription = _appLinks.uriLinkStream.listen((uri) async {
      if (uri == lastUri) return; // Debouncing Deep Links
      lastUri = uri;
      switch (uri.host) {
        case "add-repo":
          final repoName = uri.queryParameters["repo_name"];
          final repoUrl = uri.queryParameters["repo_url"];
          final mangaRepoUrls = uri.queryParametersAll["manga_url"];
          final animeRepoUrls = uri.queryParametersAll["anime_url"];
          final novelRepoUrls = uri.queryParametersAll["novel_url"];
          final context = navigatorKey.currentContext;
          if (context == null || !context.mounted) return;
          final l10n = context.l10n;
          // repo_name and repo_url are only labels the link chooses for
          // itself; the *_url lists are what actually gets installed. Show
          // those, or a link can borrow a trusted repo's name for its own.
          final sourcesToAdd = [
            for (final url in mangaRepoUrls ?? const <String>[])
              (l10n.manga, url),
            for (final url in animeRepoUrls ?? const <String>[])
              (l10n.anime, url),
            for (final url in novelRepoUrls ?? const <String>[])
              (l10n.novel, url),
          ];
          if (sourcesToAdd.isEmpty) return;
          showDialog(
            context: navigatorKey.currentContext!,
            builder: (BuildContext context) {
              return ShioriDialog(
                title: l10n.add_repo,
                content: SingleChildScrollView(
                  child: Column(
                    mainAxisSize: MainAxisSize.min,
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        l10n.label_value(l10n.name, repoName ?? l10n.unknown),
                      ),
                      const SizedBox(height: 8),
                      Text(l10n.label_value(l10n.url, repoUrl ?? l10n.unknown)),
                      const SizedBox(height: 16),
                      Text(l10n.add_repo_sources_to_add),
                      const SizedBox(height: 4),
                      for (final (type, url) in sourcesToAdd)
                        Padding(
                          padding: const EdgeInsets.only(top: 4),
                          child: SelectableText(l10n.label_value(type, url)),
                        ),
                      const SizedBox(height: 16),
                      Text(
                        l10n.add_repo_warning,
                        style: TextStyle(
                          color: context.shiori.color.error.foreground,
                        ),
                      ),
                    ],
                  ),
                ),
                actions: [
                  ShioriButton.quiet(
                    label: l10n.cancel,
                    onPressed: () => Navigator.of(context).pop(),
                  ),
                  ShioriButton(
                    label: l10n.add,
                    onPressed: () async {
                      if (context.mounted) Navigator.of(context).pop();

                      final validUrls = await _checkValidUrls([
                        ...mangaRepoUrls ?? [],
                        ...animeRepoUrls ?? [],
                        ...novelRepoUrls ?? [],
                      ]);

                      if (!validUrls) {
                        botToast(l10n.unsupported_repo);
                        return;
                      }

                      Future<void> addRepos(
                        ItemType type,
                        List<String>? urls,
                      ) async {
                        if (urls == null) return;
                        final current = ref.read(
                          extensionsRepoStateProvider(type),
                        );
                        final existingUrls = current
                            .map((r) => r.jsonUrl?.trim().toLowerCase())
                            .whereType<String>()
                            .toSet();
                        final newRepos = urls
                            .where((e) {
                              final clean = e.trim().toLowerCase();
                              return !existingUrls.contains(clean) &&
                                  !existingUrls.contains('$clean/') &&
                                  !existingUrls.contains(
                                    clean.endsWith('/')
                                        ? clean.substring(0, clean.length - 1)
                                        : clean,
                                  );
                            })
                            .map(
                              (e) => Repo(
                                name: repoName,
                                jsonUrl: e,
                                website: repoUrl,
                              ),
                            )
                            .toList();
                        if (newRepos.isEmpty) return;
                        final updated = [...current, ...newRepos];
                        await ref
                            .read(extensionsRepoStateProvider(type).notifier)
                            .set(updated);
                      }

                      await Future.wait([
                        addRepos(ItemType.manga, mangaRepoUrls),
                        addRepos(ItemType.anime, animeRepoUrls),
                        addRepos(ItemType.novel, novelRepoUrls),
                      ]);
                      botToast(l10n.repo_added);
                    },
                  ),
                ],
              );
            },
          );
          break;
        case "add-button":
          final buttonDataRaw = uri.queryParametersAll["button"];
          final context = navigatorKey.currentContext;
          if (context == null || !context.mounted || buttonDataRaw == null) {
            return;
          }
          final l10n = context.l10n;
          for (final buttonRaw in buttonDataRaw) {
            final Object? buttonData;
            try {
              buttonData = jsonDecode(utf8.decode(base64.decode(buttonRaw)));
            } catch (_) {
              continue;
            }
            if (buttonData is Map<String, dynamic>) {
              final customButton = CustomButton.fromJson(buttonData);
              // The button's code becomes an mpv Lua script, which can touch
              // the filesystem and spawn processes. Show every line of it
              // before it is saved, not just the title the link picked.
              final codeSections = [
                (l10n.custom_buttons_js_code, customButton.codePress),
                (l10n.custom_buttons_js_code_long, customButton.codeLongPress),
                (l10n.custom_buttons_startup, customButton.codeStartup),
              ].where((s) => s.$2?.trim().isNotEmpty ?? false);
              await showDialog(
                context: navigatorKey.currentContext!,
                builder: (BuildContext context) {
                  return ShioriDialog(
                    title: l10n.custom_buttons_add,
                    content: SingleChildScrollView(
                      child: Column(
                        mainAxisSize: MainAxisSize.min,
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(
                            l10n.label_value(
                              l10n.name,
                              customButton.title ?? l10n.unknown,
                            ),
                          ),
                          const SizedBox(height: 16),
                          Text(
                            l10n.custom_buttons_add_warning,
                            style: TextStyle(
                              color: context.shiori.color.error.foreground,
                            ),
                          ),
                          for (final (label, code) in codeSections) ...[
                            const SizedBox(height: 16),
                            Text(label),
                            const SizedBox(height: 4),
                            Container(
                              width: double.infinity,
                              padding: const EdgeInsets.all(8),
                              color: context.shiori.color.ink2,
                              child: SelectableText(
                                code!,
                                style: const TextStyle(
                                  fontFamily: 'monospace',
                                  fontSize: 12,
                                ),
                              ),
                            ),
                          ],
                        ],
                      ),
                    ),
                    actions: [
                      ShioriButton.quiet(
                        label: l10n.cancel,
                        onPressed: () => Navigator.of(context).pop(),
                      ),
                      ShioriButton(
                        label: l10n.add,
                        onPressed: () async {
                          if (context.mounted) Navigator.of(context).pop();
                          final pos = await customButtonRepository.count();
                          await customButtonRepository.save(
                            customButton
                              ..pos = pos
                              ..isFavourite = false
                              ..id = null,
                          );
                          botToast(l10n.custom_buttons_added);
                        },
                      ),
                    ],
                  );
                },
              );
            }
          }
          break;
        default:
      }
    });
  }

  Future<bool> _checkValidUrls(List<String> urls) async {
    final http = MClient.init(reqcopyWith: {'useDartHttpClient': true});
    for (final url in urls) {
      try {
        final req = await http.get(Uri.parse(url));
        final sourceList = (jsonDecode(req.body) as List).map(
          (e) => Source.fromJson(e),
        );
        if (sourceList.firstOrNull?.name == null) {
          return false;
        }
      } catch (err, stackTrace) {
        recordError(err, stack: stackTrace, source: 'main');
        return false;
      }
    }
    return true;
  }

  Future<void> _setupMpvConfig() async {
    try {
      final provider = StorageProvider();
      final dir = await provider.getMpvDirectory();
      final mpvFile = File(p.join(dir!.path, 'mpv.conf'));
      final inputFile = File(p.join(dir.path, 'input.conf'));
      final filesMissing =
          !(await mpvFile.exists()) && !(await inputFile.exists());
      if (filesMissing) {
        final bytes = await rootBundle.load("assets/mangayomi_mpv.zip");
        final archive = ZipDecoder().decodeBytes(bytes.buffer.asUint8List());
        final shadersDir = Directory(p.join(dir.path, 'shaders'));
        final scriptsDir = Directory(p.join(dir.path, 'scripts'));
        await Future.wait([
          shadersDir.create(recursive: true),
          scriptsDir.create(recursive: true),
        ]);

        final List<Future> writes = [];
        for (final file in archive.files) {
          final name = file.name;
          if (name == "mpv.conf") {
            writes.add(mpvFile.writeAsBytes(file.content));
          } else if (name == "input.conf") {
            writes.add(inputFile.writeAsBytes(file.content));
          } else if (name.startsWith("shaders/") && name.endsWith(".glsl")) {
            final shaderFile = File(p.join(shadersDir.path, p.basename(name)));
            writes.add(shaderFile.writeAsBytes(file.content));
          } else if (name.startsWith("scripts/") &&
              (name.endsWith(".js") || name.endsWith(".lua"))) {
            final scriptFile = File(p.join(scriptsDir.path, p.basename(name)));
            writes.add(scriptFile.writeAsBytes(file.content));
          }
        }
        await Future.wait(writes);
      }
    } catch (e) {
      // Best-effort: on Android the mpv config dir is in shared storage, which
      // may not be writable until the all-files permission is granted (now
      // requested lazily, not forced at launch). Skip setup rather than throw;
      // it's retried on a later launch once the directory is writable. See #740.
      if (kDebugMode) debugPrint('mpv config setup skipped: $e');
    }
  }

  Future<void> _checkTrackerRefresh() async {
    final prefs = await trackRepository.getAllPreferencesWithSyncId();
    for (final pref in prefs) {
      final temp = track.Track(
        syncId: pref.syncId,
        status: track.TrackStatus.completed,
      );
      ref
          .read(
            trackStateProvider(
              track: temp,
              itemType: null,
              widgetRef: ref,
            ).notifier,
          )
          .checkRefresh();
    }
  }
}

class _MouseBackButtonHandler extends StatelessWidget {
  final GoRouter router;
  final Widget child;

  const _MouseBackButtonHandler({required this.router, required this.child});

  @override
  Widget build(BuildContext context) {
    return Listener(
      onPointerDown: (event) {
        if (event.buttons & kBackMouseButton != 0) {
          if (router.canPop()) router.pop();
        }
      },
      child: child,
    );
  }
}

class AllowScrollBehavior extends MaterialScrollBehavior {
  // This allows the scrollable widgets to be scrolled with touch, mouse, stylus,
  // inverted stylus, trackpad, and unknown pointer devices.
  // This is useful for accessibility purposes, such as when using VoiceAccess,
  // which sends pointer events with unknown type when scrolling scrollables.
  // This is also useful for desktop platforms, where touch, stylus, and trackpad
  // interactions are common, and we want to ensure a consistent scrolling experience
  // across all devices.
  @override
  Set<PointerDeviceKind> get dragDevices => {
    PointerDeviceKind.touch,
    PointerDeviceKind.mouse,
    PointerDeviceKind.stylus,
    PointerDeviceKind.invertedStylus,
    PointerDeviceKind.trackpad,
    PointerDeviceKind.unknown,
  };
}
