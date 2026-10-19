// Raw socket probe: connects to host:port from flashvars, sends a line, expects it echoed, and reports through navigateToURL / on screen.
import flash.display.*;
import flash.events.*;
import flash.net.*;
import flash.system.*;
import flash.text.*;
import flash.utils.*;

class Sock extends Sprite {
	var tf:TextField;
	var results:Array<String> = [];
	var sent = false;
	static function main() { flash.Lib.current.addChild(new Sock()); }
	function new() {
		super();
		tf = new TextField(); tf.defaultTextFormat = new TextFormat("_sans", 12, 0x202020); tf.x = 6; tf.y = 6; tf.width = 460; tf.height = 120; tf.multiline = true; tf.wordWrap = true;
		addChild(tf);
		addEventListener(Event.ADDED_TO_STAGE, init);
	}
	function report(k:String, v:String) { results.push(k + "=" + v); tf.appendText(k + " " + v + "\n"); }
	function flush() { if (sent) return; sent = true; flash.Lib.getURL(new URLRequest("http://report.invalid/?" + StringTools.urlEncode(results.join("|"))), "_self"); }
	function init(_) {
		var p = flash.Lib.current.loaderInfo.parameters;
		var host:String = p.host; var port:Int = Std.parseInt(p.port);
		Security.loadPolicyFile("xmlsocket://" + host + ":" + port);
		var s = new Socket();
		s.addEventListener(Event.CONNECT, function(_) { report("connect", "ok"); s.writeUTFBytes("hello socket\n"); s.flush(); });
		s.addEventListener(ProgressEvent.SOCKET_DATA, function(_) { report("data", s.readUTFBytes(s.bytesAvailable).split("\n").join("|")); });
		s.addEventListener(IOErrorEvent.IO_ERROR, function(e:IOErrorEvent) { report("ioerror", e.text); flush(); });
		s.addEventListener(SecurityErrorEvent.SECURITY_ERROR, function(e:SecurityErrorEvent) { report("security", e.text); flush(); });
		s.addEventListener(Event.CLOSE, function(_) { report("close", "server closed"); flush(); });
		s.connect(host, port);
		var t = new Timer(6000, 1); t.addEventListener(TimerEvent.TIMER, function(_) { flush(); }); t.start();
	}
}
