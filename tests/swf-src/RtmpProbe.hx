// RTMP playback probe: connects to flashvars url (default rtmp://127.0.0.1/stream), plays flashvars stream (default "test") into a Video and paints a green
// square at (350,250) once the stream's clock has run for 2 seconds. The status lines show every NetConnection and NetStream event.
import flash.display.*;
import flash.events.*;
import flash.media.Video;
import flash.net.*;
import flash.text.*;

class RtmpProbe extends Sprite {
	static function main() { flash.Lib.current.addChild(new RtmpProbe()); }
	var out:TextField;
	var lines:Array<String> = [];
	var nc:NetConnection;
	var ns:NetStream;
	var video:Video;
	var passed = false;
	function new() {
		super();
		var p = flash.Lib.current.loaderInfo.parameters;
		var url:String = p.url != null ? p.url : "rtmp://127.0.0.1/stream";
		var name:String = p.stream != null ? p.stream : "test";
		video = new Video(320, 240); addChild(video);
		out = new TextField(); out.width = 480; out.height = 150; out.y = 250; out.multiline = true; addChild(out);
		nc = new NetConnection();
		nc.addEventListener(NetStatusEvent.NET_STATUS, function(e:NetStatusEvent) {
			note("nc " + e.info.code);
			if (e.info.code == "NetConnection.Connect.Success") {
				ns = new NetStream(nc);
				ns.client = { onMetaData: function(m:Dynamic) { note("metadata " + m.width + "x" + m.height); } };
				ns.addEventListener(NetStatusEvent.NET_STATUS, function(e2:NetStatusEvent) { note("ns " + e2.info.code); });
				ns.addEventListener(AsyncErrorEvent.ASYNC_ERROR, function(e2:AsyncErrorEvent) { note("async error"); });
				video.attachNetStream(ns);
				ns.play(name);
			}
		});
		nc.addEventListener(IOErrorEvent.IO_ERROR, function(e:IOErrorEvent) { note("io error " + e.text); });
		nc.addEventListener(SecurityErrorEvent.SECURITY_ERROR, function(e:SecurityErrorEvent) { note("security error " + e.text); });
		nc.addEventListener(AsyncErrorEvent.ASYNC_ERROR, function(e:AsyncErrorEvent) { note("nc async error"); });
		note("connecting to " + url);
		nc.connect(url);
		addEventListener(Event.ENTER_FRAME, tick);
	}
	function note(s:String) { lines.push(s); if (lines.length > 7) lines.shift(); }
	function tick(_) {
		var t = ns != null ? ns.time : 0.0;
		out.text = lines.join("\n") + "\ntime=" + Math.round(t * 10) / 10;
		if (!passed && t >= 2.0) {
			passed = true;
			graphics.beginFill(0x00ff00); graphics.drawRect(350, 250, 40, 40); graphics.endFill();
		}
	}
}
