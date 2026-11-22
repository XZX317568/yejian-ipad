import 'package:material_ui/material_ui.dart';
import 'package:shiori/shiori.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

class MainMobileBottomNavigation extends StatelessWidget {
  const MainMobileBottomNavigation({
    super.key,
    required this.isLongPressed,
    required this.location,
    required this.currentIndex,
    required this.dest,
    required this.route,
    required this.ref,
    required this.buildNavigationWidgetsMobile,
    required this.onDestinationSelected,
    required this.useFloatingNav,
    this.shrink = 0,
    this.onWake,
  });

  final bool isLongPressed;
  final String? location;
  final int currentIndex;
  final List<String> dest;
  final GoRouter route;
  final WidgetRef ref;
  final List<Widget> Function(WidgetRef, List<String>, BuildContext)
  buildNavigationWidgetsMobile;
  final Function(String) onDestinationSelected;
  final bool useFloatingNav;
  final double shrink;
  final VoidCallback? onWake;

  @override
  Widget build(BuildContext context) {
    if (!_isVisible(isLongPressed, location)) {
      return const SizedBox.shrink();
    }

    // The destinations keep the app's own labels, selected glyphs and update
    // badges; only their presentation changes. The dock owns the capsule,
    // underline and press response, so the Material navigation bar's blue
    // indicator is no longer used here.
    return ShioriArchiveDock(
      destinations: buildNavigationWidgetsMobile(
        ref,
        dest,
        context,
      ).cast<NavigationDestination>(),
      currentIndex: currentIndex,
      floating: useFloatingNav,
      shrink: useFloatingNav ? shrink : 0,
      onWake: onWake,
      onSelected: (index) {
        onWake?.call();
        onDestinationSelected(dest[index]);
      },
    );
  }

  static bool _isVisible(bool isLongPressed, String? location) {
    if (isLongPressed) return false;

    const validLocations = {
      '/MangaLibrary',
      '/AnimeLibrary',
      '/NovelLibrary',
      '/history',
      '/updates',
      '/browse',
      '/more',
      '/trackerLibrary',
    };

    return location == null || validLocations.contains(location);
  }
}
