//
//  GroupOperation.swift
//  MullvadVPN
//
//  Created by pronebird on 20/08/2020.
//  Copyright © 2020 Mullvad VPN AB. All rights reserved.
//

import Foundation

class GroupOperation: AsyncOperation {
    private let operationQueue = OperationQueue()
    private var children: Set<Operation> = []

    init(underlyingQueue: DispatchQueue? = nil, operations: [Operation]) {
        operationQueue.underlyingQueue = underlyingQueue
        operationQueue.isSuspended = true

        super.init()

        addChildren(operations)
    }

    deinit {
        // Cancel all operations and unsuspend the queue to let it perform a proper clean up
        operationQueue.cancelAllOperations()
        operationQueue.isSuspended = false
    }

    override func main() {
        operationQueue.isSuspended = false
    }

    override func operationDidCancel() {
        children.forEach { $0.cancel() }
    }

    func addChildren(_ operations: [Operation]) {
        synchronized {
            precondition(!self.isFinished, "Children cannot be added after the GroupOperation has finished.")

            self.children.formUnion(operations)

            let completionOperation = BlockOperation { [weak self] in
                self?._childrenDidFinish(operations)
            }

            operations.forEach { completionOperation.addDependency($0) }

            self.operationQueue.addOperations(operations, waitUntilFinished: false)
            self.operationQueue.addOperation(completionOperation)
        }
    }

    private func _childrenDidFinish(_ children: [Operation]) {
        synchronized {
            self.children.subtract(children)
            self.childrenDidFinish(children)

            if self.shouldFinishGroup() {
                self.finish()
            }
        }
    }

    func childrenDidFinish(_ children: [Operation]) {
        // no-op
    }

    func shouldFinishGroup() -> Bool {
        return synchronized {
            return self.children.isEmpty
        }
    }
}
