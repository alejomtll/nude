#import <AppKit/AppKit.h>
#import <WebKit/WebKit.h>

typedef void (*NudePreviewCallback)(void *, const unsigned char *, size_t);

@interface NudePreview : NSObject <WKNavigationDelegate>
@property (nonatomic, strong) NSURL *url;
@property (nonatomic, strong) NSWindow *window;
@property (nonatomic, strong) WKWebView *webView;
@property (nonatomic, strong) WKContentRuleListStore *store;
@property (nonatomic) void *context;
@property (nonatomic) NudePreviewCallback callback;
@property (nonatomic) BOOL finished;
@property (nonatomic) BOOL snapshotPending;
@end

@implementation NudePreview

- (void)finish:(NSData *)png {
    if (self.finished) return;
    self.finished = YES;
    self.webView.navigationDelegate = nil;
    [self.webView stopLoading];
    self.window.contentView = nil;
    self.webView = nil;
    self.window = nil;
    self.callback(self.context, png.bytes, png.length);
}

- (void)startWithRules:(NSString *)rules {
    // El timeout también libera la ventana si WebKit deja de responder.
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 6 * NSEC_PER_SEC), dispatch_get_main_queue(), ^{
        [self finish:nil];
    });
    self.store = [WKContentRuleListStore defaultStore];
    NSString *identifier = @"nude-loopback-preview";
    __weak NudePreview *weakSelf = self;
    [self.store compileContentRuleListForIdentifier:identifier encodedContentRuleList:rules
        completionHandler:^(WKContentRuleList *list, NSError *error) {
        NudePreview *capture = weakSelf;
        if (capture.finished || !capture) return;
        if (!list || error) {
            [capture finish:nil];
            return;
        }
        WKWebViewConfiguration *config = [WKWebViewConfiguration new];
        config.websiteDataStore = WKWebsiteDataStore.nonPersistentDataStore;
        config.preferences.javaScriptCanOpenWindowsAutomatically = NO;
        config.mediaTypesRequiringUserActionForPlayback = WKAudiovisualMediaTypeAll;
        [config.userContentController addContentRuleList:list];
        // WebRTC abre sockets fuera del cargador de recursos de WebKit.
        // También se deshabilitan workers para que no creen contextos sin esta restricción.
        NSString *isolation =
          @"for(const k of ['RTCPeerConnection','webkitRTCPeerConnection','RTCDataChannel','"
          @"WebTransport','Worker','SharedWorker'])"
          @"{try{Object.defineProperty(globalThis,k,{value:undefined,configurable:false});}catch(_){}}"
          @"try{Object.defineProperty(navigator,'serviceWorker',{value:undefined,configurable:false});}catch(_){}";
        [config.userContentController addUserScript:[[WKUserScript alloc] initWithSource:isolation
            injectionTime:WKUserScriptInjectionTimeAtDocumentStart forMainFrameOnly:NO]];

        NSRect frame = NSMakeRect(0, 0, 1280, 800);
        capture.window = [[NSWindow alloc] initWithContentRect:frame styleMask:NSWindowStyleMaskBorderless
            backing:NSBackingStoreBuffered defer:NO];
        capture.window.releasedWhenClosed = NO;
        capture.webView = [[WKWebView alloc] initWithFrame:frame configuration:config];
        capture.webView.navigationDelegate = capture;
        capture.window.contentView = capture.webView;
        // La ventana sirve de respaldo para renderizar. Nunca se ordena al frente.
        [capture.webView loadRequest:[NSURLRequest requestWithURL:capture.url
            cachePolicy:NSURLRequestReloadIgnoringLocalCacheData timeoutInterval:4]];
    }];
}

- (BOOL)allowsURL:(NSURL *)url {
    return url && [url.scheme isEqualToString:self.url.scheme] &&
        [url.host caseInsensitiveCompare:self.url.host] == NSOrderedSame &&
        url.host != nil && (url.port == self.url.port || [url.port isEqual:self.url.port]) &&
        url.user == nil && url.password == nil;
}

- (void)webView:(WKWebView *)view decidePolicyForNavigationAction:(WKNavigationAction *)action
    decisionHandler:(void (^)(WKNavigationActionPolicy))decide {
    BOOL allowed = action.targetFrame && [self allowsURL:action.request.URL];
    decide(allowed ? WKNavigationActionPolicyAllow : WKNavigationActionPolicyCancel);
    if (!allowed && action.targetFrame.mainFrame) [self finish:nil];
}

- (void)webView:(WKWebView *)view decidePolicyForNavigationResponse:(WKNavigationResponse *)response
    decisionHandler:(void (^)(WKNavigationResponsePolicy))decide {
    NSURLResponse *raw = response.response;
    BOOL ok = [self allowsURL:raw.URL] && response.canShowMIMEType;
    if ([raw isKindOfClass:NSHTTPURLResponse.class]) {
        ok = ok && ((NSHTTPURLResponse *)raw).statusCode < 400;
    }
    decide(ok ? WKNavigationResponsePolicyAllow : WKNavigationResponsePolicyCancel);
    if (!ok && response.forMainFrame) [self finish:nil];
}

- (void)webView:(WKWebView *)view didFailProvisionalNavigation:(WKNavigation *)navigation withError:(NSError *)error {
    [self finish:nil];
}

- (void)webView:(WKWebView *)view didFailNavigation:(WKNavigation *)navigation withError:(NSError *)error {
    [self finish:nil];
}

- (void)webView:(WKWebView *)view didFinishNavigation:(WKNavigation *)navigation {
    if (self.finished || self.snapshotPending) return;
    self.snapshotPending = YES;
    __weak NudePreview *weakSelf = self;
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 350 * NSEC_PER_MSEC), dispatch_get_main_queue(), ^{
        NudePreview *capture = weakSelf;
        if (!capture || capture.finished) return;
        WKSnapshotConfiguration *options = [WKSnapshotConfiguration new];
        options.rect = capture.webView.bounds;
        options.snapshotWidth = @960;
        [capture.webView takeSnapshotWithConfiguration:options completionHandler:^(NSImage *image, NSError *error) {
            NudePreview *pending = weakSelf;
            if (!pending || pending.finished) return;
            if (!image || error || image.size.width <= 0 || image.size.height <= 0) {
                [pending finish:nil];
                return;
            }
            // NSImage puede tener una representación Retina más grande que snapshotWidth.
            NSInteger width = MIN(960, ceil(image.size.width));
            NSInteger height = MAX(1, MIN(960, ceil(image.size.height * width / image.size.width)));
            NSBitmapImageRep *bitmap = [[NSBitmapImageRep alloc] initWithBitmapDataPlanes:NULL
                pixelsWide:width pixelsHigh:height bitsPerSample:8 samplesPerPixel:4 hasAlpha:YES isPlanar:NO
                colorSpaceName:NSDeviceRGBColorSpace bytesPerRow:0 bitsPerPixel:0];
            NSGraphicsContext *graphics = [NSGraphicsContext graphicsContextWithBitmapImageRep:bitmap];
            if (!graphics) {
                [pending finish:nil];
                return;
            }
            [NSGraphicsContext saveGraphicsState];
            NSGraphicsContext.currentContext = graphics;
            graphics.imageInterpolation = NSImageInterpolationHigh;
            [image drawInRect:NSMakeRect(0, 0, width, height)];
            [NSGraphicsContext restoreGraphicsState];
            [pending finish:[bitmap representationUsingType:NSBitmapImageFileTypePNG properties:@{}]];
        }];
    });
}

@end

void nude_capture_preview(const char *url, const char *rules, void *context, NudePreviewCallback callback) {
    @autoreleasepool {
        NudePreview *capture = [NudePreview new];
        capture.url = [NSURL URLWithString:[NSString stringWithUTF8String:url]];
        capture.context = context;
        capture.callback = callback;
        [capture startWithRules:[NSString stringWithUTF8String:rules]];
    }
}
