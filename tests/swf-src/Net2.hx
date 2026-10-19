// Network probe 2: child SWF via Loader, MP3 via Sound.load, binary URLLoader, 404 handling.
import flash.display.*;
import flash.events.*;
import flash.media.*;
import flash.net.*;
import flash.text.*;
import flash.utils.*;

class Net2 extends Sprite {
	var tf:TextField;
	static function main() { flash.Lib.current.addChild(new Net2()); }
	function new() {
		super();
		tf = new TextField(); tf.defaultTextFormat = new TextFormat("_sans", 12, 0x202020); tf.x = 6; tf.y = 6; tf.width = 460; tf.height = 100; tf.multiline = true; tf.wordWrap = true;
		addChild(tf);
		addEventListener(Event.ADDED_TO_STAGE, function(_) {
			var u = flash.Lib.current.loaderInfo.url;
			var o = u.substr(0, u.indexOf("/", 8));
			var l = new Loader();
			l.contentLoaderInfo.addEventListener(Event.COMPLETE, function(_) { tf.appendText("Loader ok bytes=" + l.contentLoaderInfo.bytesTotal + "\n"); l.scaleX = l.scaleY = 0.4; l.y = 110; addChild(l); });
			l.contentLoaderInfo.addEventListener(IOErrorEvent.IO_ERROR, function(e:IOErrorEvent) tf.appendText("Loader ERR " + e.text + "\n"));
			l.load(new URLRequest(o + "/tests/swf/testsrc.swf"));
			var s = new Sound();
			s.addEventListener(Event.COMPLETE, function(_) { tf.appendText("Sound ok length=" + Std.int(s.length) + "ms\n"); s.play(); });
			s.addEventListener(IOErrorEvent.IO_ERROR, function(e:IOErrorEvent) tf.appendText("Sound ERR " + e.text + "\n"));
			s.load(new URLRequest(o + "/tests/swf/tone.mp3"));
			var b = new URLLoader(); b.dataFormat = URLLoaderDataFormat.BINARY;
			b.addEventListener(Event.COMPLETE, function(_) tf.appendText("Binary ok " + (cast b.data : ByteArray).length + " bytes\n"));
			b.addEventListener(IOErrorEvent.IO_ERROR, function(e:IOErrorEvent) tf.appendText("Binary ERR " + e.text + "\n"));
			b.load(new URLRequest(o + "/tests/swf/tone.mp3"));
			var n = new URLLoader();
			n.addEventListener(Event.COMPLETE, function(_) tf.appendText("404 unexpectedly ok\n"));
			n.addEventListener(IOErrorEvent.IO_ERROR, function(e:IOErrorEvent) tf.appendText("404 -> IOError (expected)\n"));
			n.addEventListener(HTTPStatusEvent.HTTP_STATUS, function(e:HTTPStatusEvent) tf.appendText("404 status=" + e.status + "\n"));
			n.load(new URLRequest(o + "/definitely-missing.txt"));
		});
	}
}
