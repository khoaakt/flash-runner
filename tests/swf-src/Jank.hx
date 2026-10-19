// Network jank probe: an animation that records its own frame gaps while the movie downloads / parses data (the "UI freezes during a web
// request" complaint). Phases: idle baseline, 8 MB binary download, 0.9 MB RSS-like XML download + parse, 2.2 MB JSON download + parse,
// 4.8 MB PNG via Loader.load, the same PNG via Loader.loadBytes, 100 ExternalInterface round trips. Results go to the page (jankReport) and the stage.
import flash.display.*;
import flash.events.*;
import flash.external.ExternalInterface;
import flash.net.*;
import flash.text.*;
import flash.utils.*;

class Jank extends Sprite {
	static function main() { flash.Lib.current.addChild(new Jank()); }
	var box:Shape;
	var tf:TextField;
	var last:Int = 0;
	var phase:String = "";
	var origin:String;
	// per phase statistics
	var n:Int = 0; var maxGap:Int = 0; var sumGap:Int = 0; var over100:Int = 0; var stallMs:Int = 0; var t0:Int = 0;
	var rows:Array<String> = [];
	var queue:Array<Void->Void> = [];
	var eiCount:Int = 0; var eiSum:Int = 0;

	function new() {
		super();
		tf = new TextField(); tf.defaultTextFormat = new TextFormat("_sans", 11, 0x202020); tf.x = 6; tf.y = 40; tf.width = 470; tf.height = 250; tf.multiline = true; tf.wordWrap = true;
		addChild(tf);
		box = new Shape(); box.graphics.beginFill(0xd04010); box.graphics.drawRect(0, 0, 24, 24); box.graphics.endFill(); box.y = 8; addChild(box);
		addEventListener(Event.ADDED_TO_STAGE, init);
	}
	function init(_) {
		stage.frameRate = 25;
		var u = flash.Lib.current.loaderInfo.url;
		origin = u.substr(0, u.indexOf("/", 8));
		addEventListener(Event.ENTER_FRAME, frame);
		queue = [baseline, bin, xml, json, png, loadBytes, ei, finish];
		flash.Lib.current.stage.addEventListener(MouseEvent.CLICK, function(_) {});
		haxe.Timer.delay(next, 2500);   // warm up
	}
	function frame(_) {
		var t = flash.Lib.getTimer();
		box.x = (box.x + 5) % 440;
		if (last > 0 && phase != "") {
			var g = t - last;
			n++; sumGap += g; if (g > maxGap) maxGap = g; if (g > 100) over100++; if (g > 45) stallMs += g - 40;
		}
		last = t;
		if (phase == "ei" && eiCount < 100) {
			var s = flash.Lib.getTimer();
			try { ExternalInterface.call("ping", eiCount); } catch (e:Dynamic) {}
			eiSum += flash.Lib.getTimer() - s; eiCount++;
			if (eiCount == 100) { note("ei", "100 calls, avg " + Std.string(Math.round(eiSum * 10 / 100) / 10) + " ms each"); haxe.Timer.delay(next, 800); }
		}
	}
	function begin(p:String) { phase = p; n = 0; maxGap = 0; sumGap = 0; over100 = 0; stallMs = 0; t0 = flash.Lib.getTimer(); }
	function note(p:String, extra:String) {
		var el = flash.Lib.getTimer() - t0;
		var line = p + ": " + (el) + " ms, frames " + n + " (" + Std.string(Math.round(n * 10000 / Math.max(1, el)) / 10) + " fps), max gap " + maxGap + " ms, gaps>100ms " + over100 + ", stall " + stallMs + " ms" + (extra != "" ? " | " + extra : "");
		rows.push(line); tf.text = rows.join("\n");
	}
	function next() { var f = queue.shift(); if (f != null) f(); }
	function endPhase(p:String, extra:String) { haxe.Timer.delay(function() { note(p, extra); phase = ""; haxe.Timer.delay(next, 600); }, 700); }

	function baseline() { begin("baseline"); haxe.Timer.delay(function() { note("baseline", ""); phase = ""; haxe.Timer.delay(next, 300); }, 4000); }
	function bin() {
		begin("bin8MB");
		var t = flash.Lib.getTimer();
		var l = new URLLoader(); l.dataFormat = URLLoaderDataFormat.BINARY;
		l.addEventListener(Event.COMPLETE, function(_) endPhase("bin8MB", "download " + (flash.Lib.getTimer() - t) + " ms, " + (cast l.data : ByteArray).length + " bytes"));
		l.addEventListener(IOErrorEvent.IO_ERROR, function(e:IOErrorEvent) endPhase("bin8MB", "ERR " + e.text));
		l.load(new URLRequest(origin + "/tests/out/jank/big.bin?" + Std.random(1000000)));
	}
	function xml() {
		begin("xml0.9MB");
		var t = flash.Lib.getTimer();
		var l = new URLLoader();
		l.addEventListener(Event.COMPLETE, function(_) {
			var t1 = flash.Lib.getTimer();
			var cnt = -1;
			try { var x:flash.xml.XML = new flash.xml.XML(l.data); cnt = x.descendants("item").length(); } catch (e:Dynamic) { cnt = -2; }
			endPhase("xml0.9MB", "download " + (t1 - t) + " ms, parse " + (flash.Lib.getTimer() - t1) + " ms, items " + cnt);
		});
		l.addEventListener(IOErrorEvent.IO_ERROR, function(e:IOErrorEvent) endPhase("xml0.9MB", "ERR " + e.text));
		l.load(new URLRequest(origin + "/tests/out/jank/feed.xml?" + Std.random(1000000)));
	}
	function json() {
		begin("json2.2MB");
		var t = flash.Lib.getTimer();
		var l = new URLLoader();
		l.addEventListener(Event.COMPLETE, function(_) {
			var t1 = flash.Lib.getTimer();
			var cnt = -1;
			try { var a:Array<Dynamic> = haxe.Json.parse(l.data); cnt = a.length; } catch (e:Dynamic) { cnt = -2; }
			endPhase("json2.2MB", "download " + (t1 - t) + " ms, parse " + (flash.Lib.getTimer() - t1) + " ms, items " + cnt);
		});
		l.addEventListener(IOErrorEvent.IO_ERROR, function(e:IOErrorEvent) endPhase("json2.2MB", "ERR " + e.text));
		l.load(new URLRequest(origin + "/tests/out/jank/data.json?" + Std.random(1000000)));
	}
	function png() {
		begin("png4.8MB");
		var t = flash.Lib.getTimer();
		var l = new Loader();
		l.contentLoaderInfo.addEventListener(Event.COMPLETE, function(_) endPhase("png4.8MB", "load+decode " + (flash.Lib.getTimer() - t) + " ms, " + l.contentLoaderInfo.width + "x" + l.contentLoaderInfo.height));
		l.contentLoaderInfo.addEventListener(IOErrorEvent.IO_ERROR, function(e:IOErrorEvent) endPhase("png4.8MB", "ERR " + e.text));
		l.load(new URLRequest(origin + "/tests/out/jank/big.png?" + Std.random(1000000)));
	}
	function loadBytes() {
		begin("loadBytes");
		var t = flash.Lib.getTimer();
		var u = new URLLoader(); u.dataFormat = URLLoaderDataFormat.BINARY;
		u.addEventListener(Event.COMPLETE, function(_) {
			var t1 = flash.Lib.getTimer();
			var l = new Loader();
			l.contentLoaderInfo.addEventListener(Event.COMPLETE, function(_) endPhase("loadBytes", "download " + (t1 - t) + " ms, decode " + (flash.Lib.getTimer() - t1) + " ms"));
			l.contentLoaderInfo.addEventListener(IOErrorEvent.IO_ERROR, function(e:IOErrorEvent) endPhase("loadBytes", "ERR " + e.text));
			l.loadBytes(u.data);
		});
		u.addEventListener(IOErrorEvent.IO_ERROR, function(e:IOErrorEvent) endPhase("loadBytes", "ERR " + e.text));
		u.load(new URLRequest(origin + "/tests/out/jank/big.png?" + Std.random(1000000)));
	}
	function ei() { begin("ei"); eiCount = 0; eiSum = 0; }
	function finish() {
		phase = "";
		var s = rows.join("\n");
		tf.text = s + "\nDONE";
		try { ExternalInterface.call("jankReport", s); } catch (e:Dynamic) {}
	}
}
