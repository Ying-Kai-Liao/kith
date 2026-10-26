import Foundation

/// An error from the Kith cloud: `{"error":{"code","message"}}`.
public struct CloudError: LocalizedError, Sendable, Equatable {
    public var status: Int
    /// The cloud's code (`CloudErrorCode`), or one this app made (`badInvite`, `http503`, …).
    public var code: String
    public var message: String?

    public init(status: Int, code: String, message: String? = nil) {
        self.status = status
        self.code = code
        self.message = message
    }

    /// The code as the cloud contract names it; `.unknown` for one this version doesn't know or one this app made.
    public var cloudCode: CloudErrorCode { CloudErrorCode(code: code) }

    public var errorDescription: String? {
        switch cloudCode {
        case .requestExpired: "The request expired. Ask again."
        case .accountDeleted: "This account was deleted."
        case .rateLimited: "Too many requests. Try again in a little while."
        default: message ?? "Kith cloud error (\(code))"
        }
    }

    /// A busy or failing cloud, or a session token that wasn't fresh yet: trying again later
    /// settles it, so background work (launch, foreground refresh) keeps it out of the error dialog.
    public var isTransient: Bool { status >= 500 || status == 429 || status == 401 }
}

/// The user-facing `/v1` API (§8.4): account, devices, computers, access requests, claims.
/// The Clerk session token comes from the app; kit doesn't depend on ClerkKit.
public struct CloudClient: Sendable {
    public let baseURL: URL
    let identity: DeviceIdentity?
    let token: @Sendable () async throws -> String
    let session: URLSession
    /// How often `requestAccess` polls for the host's nonce.
    var pollInterval: Duration = .seconds(2)

    public init(baseURL: URL, identity: DeviceIdentity?, token: @escaping @Sendable () async throws -> String) {
        self.init(baseURL: baseURL, identity: identity, session: .shared, token: token)
    }

    init(baseURL: URL, identity: DeviceIdentity?, session: URLSession, token: @escaping @Sendable () async throws -> String) {
        self.baseURL = baseURL
        self.identity = identity
        self.session = session
        self.token = token
    }

    // MARK: account and devices


    /// Registers this device key with the account (Clerk + proof of the key).
    public func registerDevice(name: String, platform: DevicePlatform) async throws {
        let _: DeviceRegistered = try await request("POST", CloudPath.registerDevice, body: RegisterDevice(name: name, platform: platform), signed: true)
    }

    public func devices() async throws -> [CloudDevice] {
        let res: DeviceList = try await request("GET", CloudPath.listDevices)
        return res.devices
    }

    // MARK: computers

    /// With a device identity, each computer says whether this device has access.
    public func computers() async throws -> [CloudComputer] {
        let res: ComputerList = try await request("GET", CloudPath.listComputers, signed: identity != nil)
        return res.computers
    }

    /// Removes the computer from the account; every device's access to it through the account ends.
    public func removeComputer(_ id: ComputerID) async throws {
        try await send("DELETE", CloudPath.removeComputer(id))
    }

    public func grants(_ id: ComputerID) async throws -> [CloudGrant] {
        let res: GrantList = try await request("GET", CloudPath.listGrants(id))
        return res.grants
    }

    public func revokeGrant(_ id: ComputerID, grantId: String) async throws {
        try await send("DELETE", CloudPath.revokeGrant(id, grantId))
    }

    // MARK: access requests (§4.2 B, commit-then-reveal SAS)

    /// Commit → wait for the host's nonce → reveal. Returns once `code` can be shown.
    /// The device nonce lives only in memory here, so a cloud can't learn it before the host committed.
    public func requestAccess(_ id: ComputerID, signKey: String, hostVersion: String? = nil) async throws -> AccessTicket {
        guard let identity else { throw CloudError(status: 0, code: "noDeviceKey", message: "Sign in again on this device.") }
        guard let hostKey = Data(base64URL: signKey), hostKey.count == 32, RelayCrypto.computerId(signKey: hostKey) == id else {
            throw CloudError(status: 0, code: "badSignKey", message: "This computer's key doesn't match its ID.")
        }
        // A host that predates the Kith rename can only verify the commit under the old labels.
        let family = LabelFamily.forHost(version: hostVersion)
        let nD = Data.random(32)
        let commit = RelayCrypto.sasCommit(family, deviceKey: identity.deviceKey, deviceNonce: nD).base64URL
        let created: AccessRequestCreated = try await request("POST", CloudPath.createAccessRequest(id), body: CreateAccessRequest(commit: commit), signed: true)

        var hostNonce: Data?
        while hostNonce == nil {
            let status = try await accessStatus(created.requestId)
            switch status.status {
            case .pending: break
            // A state this version doesn't know is over as far as it can tell: never read as approved.
            case .expired, .unknown:
                throw CloudError(status: 0, code: CloudErrorCode.requestExpired.rawValue, message: "The computer didn't take the request.")
            case .approved, .denied, .cancelled:
                throw CloudError(status: 0, code: status.status.rawValue, message: "The computer didn't take the request.")
            }
            if let raw = status.hostNonce {
                guard let n = Data(base64URL: raw), n.count == 32 else { throw CloudError(status: 0, code: "badNonce", message: nil) }
                hostNonce = n
            } else {
                guard Int64(Date.now.timeIntervalSince1970 * 1000) < created.expiresAt else {
                    throw CloudError(status: 410, code: CloudErrorCode.requestExpired.rawValue, message: nil)
                }
                try await Task.sleep(for: pollInterval)
            }
        }
        try await send("POST", CloudPath.revealAccessRequest(created.requestId), body: Reveal(nonce: nD.base64URL), signed: true)
        let code = RelayCrypto.sasCode(family, hostSignKey: hostKey, deviceKey: identity.deviceKey, deviceNonce: nD, hostNonce: hostNonce ?? Data())
        return AccessTicket(requestId: created.requestId, expiresAt: created.expiresAt, code: code, computerId: id, signKey: signKey, labels: family)
    }

    public func accessStatus(_ requestId: String) async throws -> AccessStatus {
        try await request("GET", CloudPath.getAccessRequest(requestId))
    }

    public func cancelAccess(_ requestId: String) async throws {
        try await send("DELETE", CloudPath.cancelAccessRequest(requestId))
    }

    // MARK: invites

    /// Joins the computer an invite is for. `proof` is h1 (`InviteLink.proof`), never the token itself.
    /// The same account asking again gets the same membership.
    public func acceptInvite(_ inviteId: String, proof: String) async throws -> InviteAccepted {
        guard InviteLink.isID(inviteId), InviteLink.isProof(proof) else {
            throw CloudError(status: 0, code: "badInvite", message: "This invite link isn't valid.")
        }
        return try await request("POST", CloudPath.acceptInvite(inviteId), body: AcceptInvite(token: proof))
    }

    // MARK: claims (§4.2 A)

    // MARK: plumbing

    private struct NoBody: Encodable {}
    private struct Failure: Decodable {
        struct Detail: Decodable { var code: String; var message: String? }
        var error: Detail
    }
    private struct Ignored: Decodable {}

    private func send(_ method: String, _ path: String, body: (some Encodable)? = NoBody?.none, signed: Bool = false) async throws {
        let _: Ignored = try await request(method, path, body: body, signed: signed)
    }

    func request<T: Decodable>(_ method: String, _ path: String, body: (some Encodable)? = NoBody?.none, signed: Bool = false) async throws -> T {
        let url = baseURL.appending(path: path)
        var req = URLRequest(url: url, timeoutInterval: 20)
        req.httpMethod = method
        let data = try body.map { try JSONEncoder().encode($0) } ?? Data()
        if body != nil {
            req.httpBody = data
            req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        req.setValue("Bearer \(try await token())", forHTTPHeaderField: "Authorization")
        if signed, let identity {
            let header = try identity.signatureHeader(method: method, authority: RelayCrypto.authority(of: url),
                                                      pathAndQuery: RelayCrypto.pathAndQuery(of: url), body: data)
            req.setValue(header, forHTTPHeaderField: RelayCrypto.signatureHeaderName)
        }
        let (bytes, response) = try await session.data(for: req)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        guard (200..<300).contains(status) else {
            let e = try? JSONDecoder().decode(Failure.self, from: bytes)
            throw CloudError(status: status, code: e?.error.code ?? "http\(status)", message: e?.error.message)
        }
        return try JSONDecoder().decode(T.self, from: bytes.isEmpty ? Data("{}".utf8) : bytes)
    }
}
