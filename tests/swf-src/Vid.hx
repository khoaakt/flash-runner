// NetStream probe: plays assets/example.mp4 (H.264/AAC) next to the movie and prints every status event and the metadata.
import flash.display.*;
import flash.events.*;
import flash.media.*;
import flash.net.*;
import flash.text.*;

class Vid extends Sprite {
	var tf:TextField;
	static function main() { flash.Lib.current.addChild(new Vid()); }
	function new() {
		super();
		var v = new Video(320, 180); v.x = 10; v.y = 120; addChild(v);
		tf = new TextField(); tf.defaultTextFormat = new TextFormat("_sans", 11, 0x202020); tf.x = 6; tf.y = 4; tf.width = 380; tf.height = 112; tf.multiline = true; tf.wordWrap = true; addChild(tf);
		var nc = new NetConnection(); nc.connect(null);
		var ns = new NetStream(nc);
		ns.client = { onMetaData: function(m:Dynamic) { tf.appendText("meta " + m.width + "x" + m.height + " dur=" + m.duration + " vc=" + m.videoCodecID + " ac=" + m.audioCodecId + "\n"); } };
		ns.addEventListener(NetStatusEvent.NET_STATUS, function(e:NetStatusEvent) tf.appendText("status " + e.info.code + "\n"));
		ns.addEventListener(AsyncErrorEvent.ASYNC_ERROR, function(e:AsyncErrorEvent) tf.appendText("async " + e.text + "\n"));
		ns.addEventListener(IOErrorEvent.IO_ERROR, function(e:IOErrorEvent) tf.appendText("io " + e.text + "\n"));
		v.attachNetStream(ns);
		var u = flash.Lib.current.loaderInfo.url;
		ns.play(u.substr(0, u.lastIndexOf("/") + 1) + "assets/example.mp4");
		addEventListener(Event.ENTER_FRAME, function(_) { if (ns.time > 0 && !shown) { shown = true; tf.appendText("t=" + ns.time + " fps=" + ns.currentFPS + "\n"); } });
	}
	var shown = false;
}
