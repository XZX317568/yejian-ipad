import 'package:material_ui/material_ui.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:go_router/go_router.dart';
import 'package:mangayomi/modules/main_view/widgets/mobile_bottom_navigation.dart';
import 'package:shiori/shiori.dart';

void main() {
  late GoRouter router;

  setUp(() {
    router = GoRouter(
      routes: [
        GoRoute(path: '/', builder: (context, state) => const SizedBox()),
      ],
    );
  });

  tearDown(() => router.dispose());

  Widget app({required bool useFloatingNav}) {
    const destinations = ['/browse', '/more'];
    return ProviderScope(
      child: Consumer(
        builder: (context, ref, child) => MaterialApp(
          home: Scaffold(
            bottomNavigationBar: MainMobileBottomNavigation(
              isLongPressed: false,
              location: '/browse',
              currentIndex: 0,
              dest: destinations,
              route: router,
              ref: ref,
              useFloatingNav: useFloatingNav,
              buildNavigationWidgetsMobile: (ref, dest, context) => const [
                NavigationDestination(
                  icon: Icon(Icons.explore_outlined),
                  label: 'Browse',
                ),
                NavigationDestination(
                  icon: Icon(Icons.more_horiz),
                  label: 'More',
                ),
              ],
              onDestinationSelected: (_) {},
            ),
          ),
        ),
      ),
    );
  }

  testWidgets('uses the archive dock attached when the preference is off', (
    tester,
  ) async {
    await tester.pumpWidget(app(useFloatingNav: false));

    final dock = tester.widget<ShioriArchiveDock>(
      find.byType(ShioriArchiveDock),
    );
    expect(dock.floating, isFalse);
    expect(find.byType(NavigationBar), findsNothing);
    expect(find.text('Browse'), findsOneWidget);
    expect(find.text('More'), findsOneWidget);
  });

  testWidgets('uses the archive dock floating when the preference is on', (
    tester,
  ) async {
    await tester.pumpWidget(app(useFloatingNav: true));

    final dock = tester.widget<ShioriArchiveDock>(
      find.byType(ShioriArchiveDock),
    );
    expect(dock.floating, isTrue);
    expect(find.byType(NavigationBar), findsNothing);
    expect(find.text('Browse'), findsOneWidget);
    expect(find.text('More'), findsOneWidget);
  });
}
