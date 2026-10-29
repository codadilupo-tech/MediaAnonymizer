import 'package:flutter/material.dart';

import 'engine/engine.dart';
import 'queue.dart';
import 'settings.dart';
import 'ui/home_page.dart';

class MediaAnonymizerApp extends StatelessWidget {
  const MediaAnonymizerApp({super.key, required this.settings, required this.queue, this.engine = const Engine()});

  final SettingsController settings;
  final QueueController queue;
  final Engine engine;

  static const _seed = Color(0xFF2E5E8C);

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'MediaAnonymizer',
      debugShowCheckedModeBanner: false,
      theme: ThemeData(colorScheme: ColorScheme.fromSeed(seedColor: _seed)),
      darkTheme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: _seed, brightness: Brightness.dark),
      ),
      home: HomePage(settings: settings, queue: queue, engine: engine),
    );
  }
}
